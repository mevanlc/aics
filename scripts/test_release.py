#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.14"
# dependencies = []
# ///
"""Offline release tests: no credentials, network requests, or publication."""

import contextlib
import io
import json
import os
import tarfile
import tempfile
import tomllib
import unittest
import urllib.error
import zipfile
from pathlib import Path
from unittest import mock

import release


class Versions(unittest.TestCase):
    def test_semver(self):
        for value in (
            "0.1.0",
            "12.30.405",
            "1.0.0-rc.1",
            "1.0.0-alpha",
            "1.0.0-0",
            "1.0.0-01a",
        ):
            with self.subTest(value=value):
                self.assertEqual(release.tag_version("v" + value), value)

    def test_invalid_tags(self):
        for value in (
            "1.0.0",
            "v1.2",
            "v01.0.0",
            "v1.0.0-01",
            "v1.0.0-",
            "v1.0.0-a..b",
            "v1.0.0+build",
            "v1.0.0\n",
            "v1.0.0;echo hello",
        ):
            with self.subTest(value=value), self.assertRaises(ValueError):
                release.tag_version(value)


class ProjectTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.context = contextlib.chdir(self.root)
        self.context.__enter__()
        self.addCleanup(self.context.__exit__, None, None, None)
        Path("Cargo.toml").write_text(
            '[package]\nname = "widget"\nversion = "0.0.0" # tag placeholder\n'
            'edition = "2024"\n[dependencies]\nexample = { version = "0.0.0" }\n'
        )
        Path("Cargo.lock").write_text(
            'version = 4\n\n[[package]]\nname = "widget"\nversion = "0.0.0"\n'
            'dependencies = ["example"]\n\n[[package]]\nname = "example"\nversion = "0.0.0"\n'
            'source = "registry+https://example.invalid"\nchecksum = "abc"\n'
        )
        Path("rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.98.1"\n')
        Path("README.md").write_text("A sample project\n")
        Path("LICENSE").write_text("MIT\n")
        Path("docs").mkdir()
        Path("docs/command-line.md").write_text("Command reference\n")
        Path("examples").mkdir()
        Path("examples/log4rs.yaml").write_text("Logging example\n")
        patcher = mock.patch.object(release, "source_commit", return_value="a" * 40)
        patcher.start()
        self.addCleanup(patcher.stop)

    def make_bundle(self):
        release.stamp("0.1.0")
        Path("dist").mkdir()
        for filename in release.expected_assets("widget", "0.1.0"):
            Path("dist", filename).write_bytes(filename.encode())
        return release.bundle()

    def test_stamp_changes_only_own_version(self):
        before_manifest = tomllib.loads(Path("Cargo.toml").read_text())
        before_lock = tomllib.loads(Path("Cargo.lock").read_text())
        release.stamp("1.2.3-rc.4")
        before_manifest["package"]["version"] = "1.2.3-rc.4"
        before_lock["package"][0]["version"] = "1.2.3-rc.4"
        self.assertEqual(tomllib.loads(Path("Cargo.toml").read_text()), before_manifest)
        self.assertEqual(tomllib.loads(Path("Cargo.lock").read_text()), before_lock)
        self.assertIn("# tag placeholder", Path("Cargo.toml").read_text())

    def test_stamp_failure_leaves_both_files_untouched(self):
        Path("Cargo.lock").write_text(
            Path("Cargo.lock").read_text().replace('name = "widget"', 'name = "other"')
        )
        before = [Path(name).read_bytes() for name in ("Cargo.toml", "Cargo.lock")]
        with self.assertRaises(ValueError):
            release.stamp("1.2.3")
        self.assertEqual(
            [Path(name).read_bytes() for name in ("Cargo.toml", "Cargo.lock")], before
        )

    def test_stamp_rejects_non_placeholder(self):
        release.stamp("0.1.0")
        with self.assertRaises(ValueError):
            release.stamp("0.2.0")

    def test_archives_are_deterministic_and_complete(self):
        release.stamp("0.1.0")
        for target in (
            "x86_64-unknown-linux-musl",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-freebsd",
            "aarch64-unknown-freebsd",
        ):
            with self.subTest(target=target):
                executable = "widget.exe" if "windows" in target else "widget"
                binary = Path("target", target, "release", executable)
                binary.parent.mkdir(parents=True)
                binary.write_bytes(b"binary contents")
                with mock.patch.object(release, "run", return_value="1700000000\n"):
                    archive = release.package_binary(target)
                    first = archive.read_bytes()
                    os.utime(binary, (1800000000, 1800000000))
                    release.package_binary(target)
                self.assertEqual(first, archive.read_bytes())
                prefix = f"widget-v0.1.0-{target}/"
                expected = {
                    prefix + name
                    for name in (
                        executable,
                        "README.md",
                        "LICENSE",
                        "docs/command-line.md",
                        "examples/log4rs.yaml",
                    )
                }
                if "windows" in target:
                    with zipfile.ZipFile(archive) as stream:
                        self.assertEqual(set(stream.namelist()), expected)
                        self.assertEqual(
                            stream.read(prefix + executable), b"binary contents"
                        )
                else:
                    with tarfile.open(archive) as stream:
                        self.assertEqual(set(stream.getnames()), expected)
                        info = stream.getmember(prefix + executable)
                        self.assertEqual(info.mode, 0o755)
                        self.assertEqual(
                            (info.uid, info.gid, info.mtime), (0, 0, 1700000000)
                        )

    def test_archives_include_every_executable_and_documentation(self):
        with Path("Cargo.toml").open("a") as manifest:
            manifest.write('\n[[bin]]\nname = "first"\n[[bin]]\nname = "second"\n')
        for target in (
            "x86_64-unknown-linux-musl",
            "x86_64-pc-windows-msvc",
            "x86_64-unknown-freebsd",
            "aarch64-unknown-freebsd",
        ):
            with self.subTest(target=target):
                suffix = ".exe" if "windows" in target else ""
                build = Path("target", target, "release")
                build.mkdir(parents=True)
                for name in ("first", "second"):
                    (build / (name + suffix)).write_bytes(name.encode())
                with mock.patch.object(release, "run", return_value="1700000000\n"):
                    archive = release.package_binary(target)
                prefix = f"widget-v0.0.0-{target}/"
                expected = {
                    prefix + name
                    for name in (
                        "first" + suffix,
                        "second" + suffix,
                        "README.md",
                        "LICENSE",
                        "docs/command-line.md",
                        "examples/log4rs.yaml",
                    )
                }
                if suffix:
                    with zipfile.ZipFile(archive) as stream:
                        self.assertEqual(set(stream.namelist()), expected)
                        for name in ("first", "second"):
                            entry = stream.getinfo(prefix + name + suffix)
                            self.assertEqual(entry.external_attr >> 16, 0o100755)
                else:
                    with tarfile.open(archive) as stream:
                        self.assertEqual(set(stream.getnames()), expected)
                        for name in ("first", "second"):
                            self.assertEqual(
                                stream.getmember(prefix + name).mode, 0o755
                            )

    def test_archive_rejects_missing_executable(self):
        with Path("Cargo.toml").open("a") as manifest:
            manifest.write('\n[[bin]]\nname = "first"\n[[bin]]\nname = "second"\n')
        binary = Path("target/x86_64-unknown-linux-musl/release/first")
        binary.parent.mkdir(parents=True)
        binary.touch()
        with self.assertRaisesRegex(ValueError, "Missing binary"):
            release.package_binary("x86_64-unknown-linux-musl")

    def test_smoke_checks_each_executable_version_and_help(self):
        with Path("Cargo.toml").open("a") as manifest:
            manifest.write('\n[[bin]]\nname = "first"\n[[bin]]\nname = "second"\n')
        with mock.patch.object(
            release, "run", side_effect=["first 0.0.0\n", None, "second 0.0.0\n", None]
        ) as command:
            release.verify_binaries(Path("bin"), "x86_64-pc-windows-msvc")
        self.assertEqual(
            command.call_args_list,
            [
                mock.call(str(Path("bin/first.exe")), "--version", capture=True),
                mock.call(str(Path("bin/first.exe")), "--help"),
                mock.call(str(Path("bin/second.exe")), "--version", capture=True),
                mock.call(str(Path("bin/second.exe")), "--help"),
            ],
        )
        with (
            mock.patch.object(release, "run", return_value="first 9.9.9\n"),
            self.assertRaisesRegex(ValueError, "Unexpected binary version"),
        ):
            release.verify_binaries(Path("bin"), "x86_64-pc-windows-msvc")

    def test_bundle_checks_all_targets_and_hashes(self):
        manifest = self.make_bundle()
        self.assertEqual(release.verify_bundle(), manifest)
        filename = next(iter(manifest["assets"]))
        Path("dist", filename).write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "Checksum mismatch"):
            release.verify_bundle()

    def test_disabled_targets_do_not_block_ci_or_release_verification(self):
        entries = [
            {"target": "x86_64-unknown-linux-musl"},
            {"target": "aarch64-linux-android", "enabled": True},
            {"target": "aarch64-unknown-freebsd", "enabled": False},
        ]
        targets_file = self.root / "release-targets.json"
        targets_file.write_text(json.dumps(entries))
        with mock.patch.object(release, "TARGETS_FILE", targets_file):
            output = io.StringIO()
            with (
                mock.patch("sys.argv", ["release.py", "matrix"]),
                contextlib.redirect_stdout(output),
            ):
                release.main()
            self.assertEqual(json.loads(output.getvalue())["include"], entries[:2])
            self.assertEqual(release.targets(include_disabled=True), entries)

            release.stamp("0.1.0")
            Path("dist").mkdir()
            for filename in (
                "widget-v0.1.0-x86_64-unknown-linux-musl.tar.gz",
                "widget-v0.1.0-aarch64-linux-android.tar.gz",
                "widget-0.1.0.crate",
            ):
                Path("dist", filename).write_bytes(filename.encode())
            manifest = release.bundle()
            self.assertEqual(
                manifest["targets"], [entry["target"] for entry in entries[:2]]
            )
            self.assertEqual(release.verify_bundle(), manifest)

            entries[2]["enabled"] = True
            targets_file.write_text(json.dumps(entries))
            with self.assertRaisesRegex(ValueError, "Wrong target matrix"):
                release.verify_bundle()

    def test_incomplete_artifacts_cannot_be_bundled(self):
        release.stamp("0.1.0")
        Path("dist").mkdir()
        with self.assertRaisesRegex(ValueError, "Unexpected artifact set"):
            release.bundle()

    def test_bundle_rejects_different_commit_and_unexpected_files(self):
        self.make_bundle()
        with (
            mock.patch.object(release, "source_commit", return_value="b" * 40),
            self.assertRaisesRegex(ValueError, "different commit"),
        ):
            release.verify_bundle()
        Path("dist/unexpected").touch()
        with self.assertRaisesRegex(ValueError, "Unexpected files"):
            release.verify_bundle()

    def test_tampered_checksums_fail(self):
        self.make_bundle()
        Path("dist/SHA256SUMS").write_text("")
        with self.assertRaisesRegex(ValueError, "Checksum file mismatch"):
            release.verify_bundle()

    def test_registry_requires_exact_checksum(self):
        manifest = self.make_bundle()
        expected = manifest["assets"]["widget-0.1.0.crate"]
        with mock.patch.object(release, "crate_checksum", return_value=None):
            self.assertFalse(release.check_registry(manifest))
        with mock.patch.object(release, "crate_checksum", return_value=expected):
            self.assertTrue(release.check_registry(manifest))
        with (
            mock.patch.object(release, "crate_checksum", return_value="wrong"),
            self.assertRaisesRegex(ValueError, "different bytes"),
        ):
            release.check_registry(manifest)

    def test_partial_remote_upload_is_reusable_but_not_overwritable(self):
        manifest = self.make_bundle()
        name = next(iter(manifest["assets"]))
        remote = {"tag_name": "v0.1.0", "assets": [{"name": name}]}

        def download(*args, **kwargs):
            directory = Path(args[args.index("--dir") + 1])
            (directory / name).write_bytes(Path("dist", name).read_bytes())

        with mock.patch.object(release, "repository", return_value="owner/widget"):
            with mock.patch.object(release, "run", side_effect=download):
                missing = release.verify_remote_assets(remote, Path("dist"))
                self.assertNotIn(name, missing)
                self.assertEqual(len(missing), len(list(Path("dist").iterdir())) - 1)
                with self.assertRaisesRegex(ValueError, "incomplete"):
                    release.verify_remote_assets(remote, Path("dist"), complete=True)

            def wrong_download(*args, **kwargs):
                Path(args[args.index("--dir") + 1], name).write_bytes(b"wrong")

            with (
                mock.patch.object(release, "run", side_effect=wrong_download),
                self.assertRaisesRegex(ValueError, "Existing asset differs"),
            ):
                release.verify_remote_assets(remote, Path("dist"))

    def test_completed_release_is_a_no_op(self):
        self.make_bundle()
        completed = {
            "draft": False,
            "immutable": True,
            "html_url": "https://example.invalid/release",
        }
        with (
            mock.patch.object(release, "assert_publication", return_value="v0.1.0"),
            mock.patch.object(release, "check_registry", return_value=True),
            mock.patch.object(release, "release_state", return_value=completed),
            mock.patch.object(release, "verify_remote_assets"),
            mock.patch.object(release, "run") as command,
        ):
            release.finalize()
        command.assert_not_called()

    def test_finalization_requires_published_crate(self):
        self.make_bundle()
        with (
            mock.patch.object(release, "assert_publication", return_value="v0.1.0"),
            mock.patch.object(release, "check_registry", return_value=False),
            mock.patch.object(release, "run") as command,
            self.assertRaisesRegex(ValueError, "Crate must be published"),
        ):
            release.finalize()
        command.assert_not_called()

    def test_prerelease_finalization_is_never_latest(self):
        self.make_bundle()
        # Exercise finalization policy independently of bundle validation.
        with (
            mock.patch.object(
                release, "verify_bundle", return_value={"version": "0.2.0-rc.1"}
            ),
            mock.patch.object(
                release, "assert_publication", return_value="v0.2.0-rc.1"
            ),
            mock.patch.object(release, "check_registry", return_value=True),
            mock.patch.object(
                release,
                "release_state",
                side_effect=[
                    {"draft": True},
                    {
                        "draft": False,
                        "immutable": True,
                        "html_url": "https://example.invalid/release",
                    },
                ],
            ),
            mock.patch.object(release, "verify_remote_assets"),
            mock.patch.object(release, "repository", return_value="owner/widget"),
            mock.patch.object(release, "run") as command,
        ):
            release.finalize()
        self.assertIn("--prerelease", command.call_args.args)
        self.assertIn("--latest=false", command.call_args.args)

    def test_existing_crate_is_not_uploaded_again(self):
        self.make_bundle()
        with (
            mock.patch.object(release, "assert_publication", return_value="v0.1.0"),
            mock.patch.object(release, "release_state", return_value={}),
            mock.patch.object(release, "verify_remote_assets"),
            mock.patch.object(release, "check_registry", return_value=True),
            mock.patch.object(release.subprocess, "run") as command,
        ):
            release.publish_crate()
        command.assert_not_called()

    def test_upload_timeout_is_reconciled_against_registry(self):
        self.make_bundle()
        Path("target/package").mkdir(parents=True)
        Path("target/package/widget-0.1.0.crate").write_bytes(
            Path("dist/widget-0.1.0.crate").read_bytes()
        )
        with (
            mock.patch.object(release, "assert_publication", return_value="v0.1.0"),
            mock.patch.object(release, "release_state", return_value={"draft": True}),
            mock.patch.object(release, "verify_remote_assets"),
            mock.patch.object(release, "check_registry", side_effect=[False, True]),
            mock.patch.object(
                release.subprocess, "run", return_value=mock.Mock(returncode=101)
            ) as command,
        ):
            release.publish_crate()
        command.assert_called_once()
        self.assertEqual(command.call_args.args[0][:2], ["cargo", "publish"])

    def test_staging_resumes_only_missing_assets(self):
        self.make_bundle()
        draft = {"draft": True, "prerelease": False}
        with (
            mock.patch.object(release, "assert_publication", return_value="v0.1.0"),
            mock.patch.object(release, "release_state", return_value=draft),
            mock.patch.object(
                release, "verify_remote_assets", side_effect=[{"SHA256SUMS"}, set()]
            ),
            mock.patch.object(release, "repository", return_value="owner/widget"),
            mock.patch.object(release, "run") as command,
        ):
            release.stage()
        command.assert_called_once_with(
            "gh",
            "release",
            "upload",
            "v0.1.0",
            "--repo",
            "owner/widget",
            str(Path("dist/SHA256SUMS")),
        )

    def test_staging_waits_for_created_draft_without_creating_twice(self):
        self.make_bundle()
        draft = {"draft": True, "prerelease": False}
        with (
            mock.patch.object(release, "assert_publication", return_value="v0.1.0"),
            mock.patch.object(
                release, "release_state", side_effect=[None, None, None, draft, draft]
            ),
            mock.patch.object(release, "verify_remote_assets", return_value=set()),
            mock.patch.object(release, "repository", return_value="owner/widget"),
            mock.patch.object(release, "run") as command,
            mock.patch.object(release.time, "sleep") as sleep,
        ):
            release.stage()
        command.assert_called_once()
        self.assertEqual(command.call_args.args[:3], ("gh", "release", "create"))
        self.assertEqual(sleep.call_args_list, [mock.call(1), mock.call(2)])

    def test_staging_visibility_retry_is_bounded(self):
        self.make_bundle()
        with (
            mock.patch.object(release, "assert_publication", return_value="v0.1.0"),
            mock.patch.object(release, "release_state", return_value=None) as state,
            mock.patch.object(release, "repository", return_value="owner/widget"),
            mock.patch.object(release, "run") as command,
            mock.patch.object(release.time, "sleep"),
            self.assertRaisesRegex(ValueError, "Created draft is not visible"),
        ):
            release.stage()
        command.assert_called_once()
        self.assertEqual(state.call_count, 7)

    def test_server_digest_is_checked_without_redownloading(self):
        self.make_bundle()
        name = "widget-0.1.0.crate"
        remote = {
            "assets": [
                {"name": name, "digest": "sha256:" + release.digest(Path("dist", name))}
            ]
        }
        with mock.patch.object(release, "run") as command:
            release.verify_remote_assets(remote, Path("dist"))
            remote["assets"][0]["digest"] = "sha256:wrong"
            with self.assertRaisesRegex(ValueError, "Existing asset differs"):
                release.verify_remote_assets(remote, Path("dist"))
        command.assert_not_called()


class GitHubAndNetwork(unittest.TestCase):
    def test_finds_a_draft_through_paginated_listing(self):
        draft = {"tag_name": "v1.0.0", "draft": True}
        with mock.patch.object(
            release,
            "github",
            side_effect=[
                None,
                [{"tag_name": "v0.1.0"}] * 100,
                [draft],
            ],
        ) as request:
            self.assertEqual(release.release_state("v1.0.0"), draft)
        self.assertEqual(request.call_args.args[0], "releases?per_page=100&page=2")

    def test_manual_and_branch_runs_cannot_publish(self):
        for environment in (
            {"GITHUB_ACTIONS": "true", "GITHUB_EVENT_NAME": "workflow_dispatch"},
            {
                "GITHUB_ACTIONS": "true",
                "GITHUB_EVENT_NAME": "push",
                "GITHUB_REF_NAME": "v1.0.0",
                "GITHUB_REF": "refs/heads/v1.0.0",
            },
        ):
            with (
                self.subTest(environment=environment),
                mock.patch.dict(os.environ, environment, clear=True),
                self.assertRaises(ValueError),
            ):
                release.assert_publication()

    def test_moved_tag_is_rejected(self):
        environment = {
            "GITHUB_ACTIONS": "true",
            "GITHUB_EVENT_NAME": "push",
            "GITHUB_REF_NAME": "v1.0.0",
            "GITHUB_REF": "refs/tags/v1.0.0",
        }
        with (
            mock.patch.dict(os.environ, environment, clear=True),
            mock.patch.object(
                release, "project", return_value=({"version": "1.0.0"}, "widget")
            ),
            mock.patch.object(
                release,
                "github",
                return_value={"object": {"type": "commit", "sha": "wrong"}},
            ),
            mock.patch.object(release, "source_commit", return_value="expected"),
            self.assertRaisesRegex(ValueError, "tag moved"),
        ):
            release.assert_publication()

    def test_only_404_means_absent(self):
        for code in (401, 403, 404, 500):
            error = urllib.error.HTTPError(
                "https://example.invalid", code, "test", {}, None
            )
            self.addCleanup(error.close)
            with (
                self.subTest(code=code),
                mock.patch.object(release.urllib.request, "urlopen", side_effect=error),
                mock.patch.object(release.time, "sleep"),
            ):
                if code == 404:
                    self.assertIsNone(
                        release.json_request("https://example.invalid", missing_ok=True)
                    )
                else:
                    with self.assertRaises(urllib.error.HTTPError):
                        release.json_request("https://example.invalid", missing_ok=True)


if __name__ == "__main__":
    unittest.main()
