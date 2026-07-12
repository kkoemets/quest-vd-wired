param(
    [Parameter(Mandatory = $true)]
    [string]$ApkPath,
    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,
    [Parameter(Mandatory = $true)]
    [string]$AndroidArtifactsDirectory
)

$ErrorActionPreference = "Stop"
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$manifest = Join-Path $repoRoot "host-rust\Cargo.toml"
$apk = (Resolve-Path $ApkPath).Path
$androidArtifacts = (Resolve-Path $AndroidArtifactsDirectory).Path
$output = [System.IO.Path]::GetFullPath($OutputDirectory)
$target = "x86_64-pc-windows-msvc"
$builtExe = Join-Path $repoRoot "host-rust\target\$target\release\gnirehtet-vd.exe"

function Invoke-Checked {
    param([string]$Command, [string[]]$Arguments)
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Command failed with exit code $LASTEXITCODE"
    }
}

foreach ($command in @("cargo", "python")) {
    if (-not (Get-Command $command -ErrorAction SilentlyContinue)) {
        throw "required command is unavailable: $command"
    }
}
if (-not (Test-Path -LiteralPath $apk -PathType Leaf)) {
    throw "signed Android v4 APK is missing"
}

if (Test-Path -LiteralPath $output) {
    Remove-Item -LiteralPath $output -Recurse -Force
}
New-Item -ItemType Directory -Path $output | Out-Null

$env:GNIREHTET_VD_APK = $apk
$env:CARGO_INCREMENTAL = "0"
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS = "-C target-feature=+crt-static -C link-arg=/Brepro"
if (-not $env:SOURCE_DATE_EPOCH) {
    $env:SOURCE_DATE_EPOCH = (& git -C $repoRoot show -s --format=%ct HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw "could not determine SOURCE_DATE_EPOCH" }
}

Invoke-Checked "cargo" @("fmt", "--manifest-path", $manifest, "--all", "--", "--check")
Invoke-Checked "cargo" @("deny", "--manifest-path", $manifest, "--config", (Join-Path $repoRoot "host-rust\deny.toml"), "check")
Invoke-Checked "cargo" @("clippy", "--manifest-path", $manifest, "--locked", "--target", $target, "--all-targets", "--", "-D", "warnings")
Invoke-Checked "cargo" @("test", "--manifest-path", $manifest, "--locked", "--target", $target, "--all-targets")
Invoke-Checked "cargo" @("build", "--manifest-path", $manifest, "--locked", "--target", $target, "--release")
Invoke-Checked "python" @((Join-Path $repoRoot "scripts\normalize_windows_pe.py"), $builtExe)

if (-not (Test-Path -LiteralPath $builtExe -PathType Leaf)) {
    throw "Windows x64 host executable was not produced"
}
$firstExe = Join-Path $output "gnirehtet-vd.exe"
Copy-Item -LiteralPath $builtExe -Destination $firstExe
Invoke-Checked "python" @((Join-Path $repoRoot "scripts\verify_embedded_apk.py"), $firstExe, $apk)

Invoke-Checked "cargo" @("clean", "--manifest-path", $manifest, "--target", $target)
Invoke-Checked "cargo" @("build", "--manifest-path", $manifest, "--locked", "--target", $target, "--release")
Invoke-Checked "python" @((Join-Path $repoRoot "scripts\normalize_windows_pe.py"), $builtExe)
$firstHash = (Get-FileHash -LiteralPath $firstExe -Algorithm SHA256).Hash.ToLowerInvariant()
$secondHash = (Get-FileHash -LiteralPath $builtExe -Algorithm SHA256).Hash.ToLowerInvariant()
@(
    "artifact=gnirehtet-vd.exe"
    "first_sha256=$firstHash"
    "second_sha256=$secondHash"
    "reproducible=$($firstHash -eq $secondHash)".ToLowerInvariant()
) | Set-Content -LiteralPath (Join-Path $output "WINDOWS_REPRODUCIBILITY.txt") -Encoding utf8
if ($firstHash -ne $secondHash) {
    throw "Windows x64 host is not reproducible across clean builds"
}
Invoke-Checked "python" @((Join-Path $repoRoot "scripts\verify_embedded_apk.py"), $builtExe, $apk)

Invoke-Checked "cargo" @(
    "cyclonedx",
    "--manifest-path", $manifest,
    "--format", "json",
    "--all",
    "--target", $target,
    "--spec-version", "1.5",
    "--override-filename", "gnirehtet-vd.cdx"
)
$sboms = @(Get-ChildItem -LiteralPath (Join-Path $repoRoot "host-rust") -Recurse -Filter "gnirehtet-vd*.cdx.json")
if ($sboms.Count -ne 1) {
    throw "expected exactly one Rust CycloneDX SBOM, found $($sboms.Count)"
}
Copy-Item -LiteralPath $sboms[0].FullName -Destination (Join-Path $output "gnirehtet-vd-rust.cdx.json")
Invoke-Checked "python" @(
    (Join-Path $repoRoot "scripts\generate_rust_notices.py"),
    "--manifest", $manifest,
    "--output", (Join-Path $output "RUST_THIRD_PARTY_NOTICES.md")
)
Copy-Item -LiteralPath $apk -Destination (Join-Path $output "gnirehtet-v4.apk")
Copy-Item -LiteralPath (Join-Path $repoRoot "LICENSE") -Destination (Join-Path $output "PROJECT_LICENSE.txt")
$androidEvidence = Join-Path $output "android"
New-Item -ItemType Directory -Path $androidEvidence | Out-Null
Copy-Item -Path (Join-Path $androidArtifacts "*") -Destination $androidEvidence -Recurse
Invoke-Checked "python" @(
    (Join-Path $repoRoot "scripts\write_sha256.py"),
    $output,
    "--output", (Join-Path $output "SHA256SUMS")
)
