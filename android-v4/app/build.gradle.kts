import org.gradle.api.tasks.Exec
import java.io.File

plugins {
    id("com.android.application")
}

val hevRevision = "c6e4c72246fb0f20bda299f0efc7814bb3098d57"
val releaseStorePath = providers.gradleProperty("RELEASE_STORE_FILE")
val releaseStorePassword = providers.gradleProperty("RELEASE_STORE_PASSWORD")
val releaseKeyAlias = providers.gradleProperty("RELEASE_KEY_ALIAS")
val releaseKeyPassword = providers.gradleProperty("RELEASE_KEY_PASSWORD")
val releaseSigningReady = listOf(
    releaseStorePath,
    releaseStorePassword,
    releaseKeyAlias,
    releaseKeyPassword,
).all { it.isPresent }

fun resolveSigningStore(configuredPath: String): File {
    val configured = File(configuredPath)
    if (configured.isAbsolute) return configured
    // The final candidate preserves the path semantics used by the legacy app/
    // Gradle project so the same private key can sign in-place v3 -> v4 upgrades.
    return listOf(
        project.file(configuredPath),
        rootProject.file(configuredPath),
        rootProject.file("../app/$configuredPath"),
    ).firstOrNull(File::isFile) ?: project.file(configuredPath)
}

val prepareHev by tasks.registering(Exec::class) {
    inputs.property("revision", hevRevision)
    outputs.dir(rootProject.layout.projectDirectory.dir(".deps/hev-socks5-tunnel"))
    commandLine("bash", rootProject.file("scripts/fetch-hev.sh"), hevRevision)
}

android {
    namespace = "com.genymobile.gnirehtet.v4"
    compileSdk = 36
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "com.genymobile.gnirehtet"
        minSdk = 29
        targetSdk = 36
        versionCode = 42
        versionName = "4.0.0-beta.3"

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        ndk {
            abiFilters += "arm64-v8a"
        }
        externalNativeBuild {
            ndkBuild {
                arguments += listOf(
                    "APP_CFLAGS+=-DPKGNAME=hev/htproxy -DCLSNAME=TProxyService -ffile-prefix-map=${rootDir}=.",
                    "APP_LDFLAGS+=-Wl,--build-id=none",
                )
            }
        }
    }

    signingConfigs {
        if (releaseSigningReady) {
            create("release") {
                storeFile = resolveSigningStore(releaseStorePath.get())
                storePassword = releaseStorePassword.get()
                keyAlias = releaseKeyAlias.get()
                keyPassword = releaseKeyPassword.get()
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            if (releaseSigningReady) {
                signingConfig = signingConfigs.getByName("release")
            }
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    externalNativeBuild {
        ndkBuild {
            path = file("src/main/jni/Android.mk")
        }
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }

    sourceSets {
        getByName("test").resources.directories.add(rootProject.file("../protocol/fixtures").absolutePath)
    }

    lint {
        // These versions and the single arm64 ABI are deliberate v4 product constraints.
        disable += setOf("AndroidGradlePluginVersion", "ChromeOsAbiSupport", "GradleDependency", "OldTargetApi")
    }
}

tasks.configureEach {
    if (name.contains("NdkBuild", ignoreCase = true)) {
        dependsOn(prepareHev)
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
}
