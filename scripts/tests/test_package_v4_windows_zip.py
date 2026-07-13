from __future__ import annotations

import datetime as dt
import os
import subprocess
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

import package_v4_windows_zip  # noqa: E402


class DeterministicWindowsZipTest(unittest.TestCase):
    def test_two_runs_are_byte_identical_with_normalized_metadata(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            staging = root / "staging"
            staging.mkdir()
            (staging / "z.txt").write_text("last\n", encoding="utf-8")
            android = staging / "android"
            android.mkdir()
            (android / "artifact.txt").write_text("android\n", encoding="utf-8")
            (staging / "a.txt").write_text("first\n", encoding="utf-8")
            first = root / "first.zip"
            second = root / "second.zip"
            epoch = 1_700_000_001

            package_v4_windows_zip.package(staging, first, source_date_epoch=epoch)
            os.chmod(staging / "a.txt", 0o777)
            os.utime(staging / "a.txt", (1_800_000_000, 1_800_000_000))
            package_v4_windows_zip.package(staging, second, source_date_epoch=epoch)

            self.assertEqual(first.read_bytes(), second.read_bytes())
            with zipfile.ZipFile(first) as archive:
                infos = archive.infolist()
                self.assertEqual(
                    ["a.txt", "android/", "android/artifact.txt", "z.txt"],
                    [info.filename for info in infos],
                )
                expected_time = dt.datetime.fromtimestamp(epoch, tz=dt.timezone.utc)
                expected_tuple = (
                    expected_time.year,
                    expected_time.month,
                    expected_time.day,
                    expected_time.hour,
                    expected_time.minute,
                    expected_time.second - expected_time.second % 2,
                )
                for info in infos:
                    self.assertEqual(expected_tuple, info.date_time)
                    self.assertEqual(3, info.create_system)
                    self.assertEqual(b"", info.extra)
                    self.assertEqual(b"", info.comment)
                files = [info for info in infos if not info.is_dir()]
                directories = [info for info in infos if info.is_dir()]
                self.assertTrue(all((info.external_attr >> 16) & 0o777 == 0o644 for info in files))
                self.assertTrue(
                    all((info.external_attr >> 16) & 0o777 == 0o755 for info in directories)
                )
                self.assertTrue(all(info.compress_type == zipfile.ZIP_DEFLATED for info in files))
                self.assertTrue(all(info.compress_type == zipfile.ZIP_STORED for info in directories))

    def test_source_date_epoch_environment_and_1980_bound(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            staging = root / "staging"
            staging.mkdir()
            (staging / "release.txt").write_text("release", encoding="utf-8")
            output = root / "release.zip"
            environment = os.environ.copy()
            environment["SOURCE_DATE_EPOCH"] = "0"

            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPTS / "package_v4_windows_zip.py"),
                    str(staging),
                    str(output),
                ],
                check=True,
                env=environment,
            )

            with zipfile.ZipFile(output) as archive:
                self.assertEqual((1980, 1, 1, 0, 0, 0), archive.infolist()[0].date_time)

    def test_explicit_epoch_overrides_environment(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            staging = root / "staging"
            staging.mkdir()
            (staging / "release.txt").write_text("release", encoding="utf-8")
            output = root / "release.zip"
            package_v4_windows_zip.package(
                staging,
                output,
                source_date_epoch=1_700_000_001,
                environment={"SOURCE_DATE_EPOCH": "0"},
            )
            with zipfile.ZipFile(output) as archive:
                self.assertNotEqual((1980, 1, 1, 0, 0, 0), archive.infolist()[0].date_time)

    def test_existing_empty_android_directory_is_preserved_but_not_invented(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with_android = root / "with-android"
            with_android.mkdir()
            (with_android / "gnirehtet-vd.exe").write_bytes(b"MZ")
            (with_android / "android").mkdir()
            first = root / "with.zip"
            package_v4_windows_zip.package(with_android, first, source_date_epoch=0)
            with zipfile.ZipFile(first) as archive:
                self.assertIn("android/", archive.namelist())

            without_android = root / "without-android"
            without_android.mkdir()
            (without_android / "gnirehtet-vd.exe").write_bytes(b"MZ")
            second = root / "without.zip"
            package_v4_windows_zip.package(without_android, second, source_date_epoch=0)
            with zipfile.ZipFile(second) as archive:
                self.assertNotIn("android/", archive.namelist())

    def test_output_inside_staging_directory_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            staging = Path(directory) / "staging"
            staging.mkdir()
            (staging / "release.txt").write_text("release", encoding="utf-8")
            output = staging / "release.zip"
            with self.assertRaisesRegex(
                package_v4_windows_zip.PackagingError,
                "outside the staging directory",
            ):
                package_v4_windows_zip.package(staging, output, source_date_epoch=0)
            self.assertFalse(output.exists())

    def test_symlinks_and_windows_unsafe_names_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            staging = root / "staging"
            staging.mkdir()
            source = root / "source.txt"
            source.write_text("release", encoding="utf-8")
            (staging / "link.txt").symlink_to(source)
            with self.assertRaisesRegex(package_v4_windows_zip.PackagingError, "symbolic links"):
                package_v4_windows_zip.package(staging, root / "release.zip", source_date_epoch=0)

        for name in ("CON.txt", "bad:name", "trailing. "):
            with self.subTest(name=name):
                with self.assertRaises(package_v4_windows_zip.PackagingError):
                    package_v4_windows_zip._validate_component(name)


if __name__ == "__main__":
    unittest.main()
