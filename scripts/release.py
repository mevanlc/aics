#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.14"
# dependencies = []
# ///
"""Verified aics releases. See devdocs/RELEASING.md."""

import argparse
import datetime
import gzip
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

NUMBER = r"(?:0|[1-9][0-9]*)"
IDENTIFIER = r"(?:0|[1-9][0-9]*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)"
VERSION = re.compile(
    rf"{NUMBER}\.{NUMBER}\.{NUMBER}(?:-{IDENTIFIER}(?:\.{IDENTIFIER})*)?"
)
TARGETS_FILE = Path(__file__).with_name("release-targets.json")


def run(*args, capture=False, cwd=None):
    return subprocess.run(
        args,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        cwd=cwd,
    ).stdout


def require(condition, message):
    if not condition:
        raise ValueError(message)


def version(value):
    require(VERSION.fullmatch(value) is not None, f"Invalid release version: {value!r}")
    return value


def tag_version(tag):
    require(tag.startswith("v"), "Release tags must start with v")
    return version(tag[1:])


def project(root=Path(".")):
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    package = manifest["package"]
    require(
        "workspace" not in manifest,
        "This exemplar supports a single package, not a workspace",
    )
    bins = [entry["name"] for entry in manifest.get("bin", [{"name": package["name"]}])]
    require(bins and len(set(bins)) == len(bins), "Expected distinct binary names")
    return package, bins


def targets():
    return json.loads(TARGETS_FILE.read_text())


def toolchain():
    return tomllib.loads(Path("rust-toolchain.toml").read_text())["toolchain"][
        "channel"
    ]


def source_commit():
    return run("git", "rev-parse", "HEAD", capture=True).strip()


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def stamp(release_version, root=Path(".")):
    version(release_version)
    package, _ = project(root)
    require(package["version"] == "0.0.0", "Committed package version must be 0.0.0")
    manifest_path, lock_path = root / "Cargo.toml", root / "Cargo.lock"
    manifest_text, lock_text = manifest_path.read_text(), lock_path.read_text()
    old_manifest, old_lock = tomllib.loads(manifest_text), tomllib.loads(lock_text)
    entries = [
        p
        for p in old_lock["package"]
        if p["name"] == package["name"] and "source" not in p
    ]
    require(
        len(entries) == 1 and entries[0]["version"] == "0.0.0",
        "Expected one local 0.0.0 lockfile entry",
    )

    def replace_section(text, section, predicate):
        parts = re.split(r"(?m)(?=^\[\[?[^ \n])", text)
        count = 0
        for index, part in enumerate(parts):
            if part.startswith(section + "\n") and predicate(tomllib.loads(part)):
                parts[index], changed = re.subn(
                    r'(?m)^(version\s*=\s*)"0\.0\.0"(\s*(?:#.*)?)$',
                    lambda match: f'{match[1]}"{release_version}"{match[2]}',
                    part,
                )
                require(
                    changed == 1,
                    "Expected exactly one placeholder in the selected section",
                )
                count += 1
        require(count == 1, f"Expected exactly one {section} section")
        return "".join(parts)

    stamped_manifest = replace_section(manifest_text, "[package]", lambda _: True)
    stamped_lock = replace_section(
        lock_text,
        "[[package]]",
        lambda data: (
            data["package"][0]["name"] == package["name"]
            and "source" not in data["package"][0]
        ),
    )
    old_manifest["package"]["version"] = release_version
    entries[0]["version"] = release_version
    require(
        tomllib.loads(stamped_manifest) == old_manifest,
        "Stamping changed unrelated manifest fields",
    )
    require(
        tomllib.loads(stamped_lock) == old_lock,
        "Stamping changed dependency resolution",
    )
    # Validate both files before replacing either.
    manifest_path.write_text(stamped_manifest)
    lock_path.write_text(stamped_lock)


def archive_name(package, release_version, target):
    extension = "zip" if "windows" in target else "tar.gz"
    return f"{package}-v{release_version}-{target}.{extension}"


def package_binary(target, destination=Path("dist")):
    package, binaries = project()
    require(
        target in {entry["target"] for entry in targets()}, f"Unknown target: {target}"
    )
    release_version = version(package["version"])
    executables = {
        binary + (".exe" if "windows" in target else "") for binary in binaries
    }
    files = {
        **{name: Path("target") / target / "release" / name for name in executables},
        "README.md": Path("README.md"),
        "LICENSE": Path("LICENSE"),
    }
    for directory in ("docs", "examples"):
        files.update(
            (path.as_posix(), path)
            for path in sorted(Path(directory).rglob("*"))
            if path.is_file()
        )
    require(
        all(path.is_file() for path in files.values()),
        "Missing binary or required documentation",
    )
    epoch = int(run("git", "show", "-s", "--format=%ct", "HEAD", capture=True).strip())
    prefix = f"{package['name']}-v{release_version}-{target}"
    destination.mkdir(parents=True, exist_ok=True)
    output = destination / archive_name(package["name"], release_version, target)
    if "windows" in target:
        date = datetime.datetime.fromtimestamp(max(epoch, 315532800), datetime.UTC)
        with zipfile.ZipFile(
            output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9
        ) as archive:
            for name, path in sorted(files.items()):
                info = zipfile.ZipInfo(f"{prefix}/{name}", date.timetuple()[:6])
                info.create_system = 3
                info.external_attr = (
                    0o100755 if name in executables else 0o100644
                ) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, path.read_bytes())
    else:
        with (
            output.open("wb") as stream,
            gzip.GzipFile(
                filename="", fileobj=stream, mode="wb", mtime=epoch
            ) as compressed,
            tarfile.open(fileobj=compressed, mode="w") as archive,
        ):
            for name, path in sorted(files.items()):
                content = path.read_bytes()
                info = tarfile.TarInfo(f"{prefix}/{name}")
                info.size, info.mtime = len(content), epoch
                info.mode = 0o755 if name in executables else 0o644
                archive.addfile(info, io.BytesIO(content))
    return output


def smoke(target):
    package, _ = project()
    archive = Path("dist") / archive_name(package["name"], package["version"], target)
    with tempfile.TemporaryDirectory() as directory:
        if archive.suffix == ".zip":
            with zipfile.ZipFile(archive) as stream:
                stream.extractall(directory)
        else:
            with tarfile.open(archive) as stream:
                stream.extractall(directory, filter="data")
        verify_binaries(
            Path(directory) / f"{package['name']}-v{package['version']}-{target}",
            target,
        )
        smoke_search(
            Path(directory)
            / f"{package['name']}-v{package['version']}-{target}"
            / ("aics.exe" if "windows" in target else "aics")
        )


def smoke_search(executable):
    executable = str(executable.resolve())
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        claude, codex = root / "claude", root / "codex"
        (claude / "project").mkdir(parents=True)
        codex.mkdir()
        shutil.copyfile(
            "tests/fixtures/sessions/claude/basic_session.jsonl",
            claude / "project/session.jsonl",
        )
        env = {
            **os.environ,
            "AICS_CONFIG_ROOT": str(root / "config"),
            "AICS_CACHE_ROOT": str(root / "cache"),
            "AICS_DATA_ROOT": str(root / "data"),
            "AICS_CLAUDE_PROJECTS_DIR": str(claude),
            "AICS_CODEX_SESSIONS_DIR": str(codex),
            "AICS_ANTIGRAVITY_HOME": str(root / "antigravity"),
        }
        manual = subprocess.run(
            [executable, "--manpage"],
            check=True,
            capture_output=True,
            text=True,
            env=env,
        ).stdout
        require(bool(manual.strip()), "Packaged binary must contain the manual")
        result = subprocess.run(
            [
                executable,
                "--json",
                "-g",
                "--agent",
                "claude",
                "--progress",
                "none",
                "git status",
            ],
            check=True,
            capture_output=True,
            text=True,
            env=env,
        )
        hits = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
        require(len(hits) == 1, "Smoke search must find exactly one fixture session")
        require(
            hits[0]["session"]["agent"] == "claude"
            and hits[0]["session"]["session_id"]
            == "c0d1e2f3-a4b5-4c6d-8e7f-9a0b1c2d3e4f",
            "Smoke search returned the wrong fixture session",
        )
        print("Packaged binary indexed and searched the isolated fixture successfully")


def verify_binaries(directory, target):
    package, binaries = project()
    for binary in binaries:
        executable = directory / (binary + (".exe" if "windows" in target else ""))
        actual = run(str(executable), "--version", capture=True).strip()
        require(
            actual == f"{binary} {package['version']}",
            f"Unexpected binary version: {actual}",
        )
        run(str(executable), "--help")


def expected_assets(package, release_version):
    return {
        archive_name(package, release_version, entry["target"]) for entry in targets()
    } | {f"{package}-{release_version}.crate"}


def bundle(directory=Path("dist")):
    package, _ = project()
    release_version = version(package["version"])
    expected = expected_assets(package["name"], release_version)
    actual = {path.name for path in directory.iterdir()}
    require(
        actual == expected,
        f"Unexpected artifact set: missing {expected - actual}, extra {actual - expected}",
    )
    manifest = {
        "schema": 1,
        "package": package["name"],
        "version": release_version,
        "commit": source_commit(),
        "toolchain": toolchain(),
        "targets": [entry["target"] for entry in targets()],
        "assets": {name: digest(directory / name) for name in sorted(expected)},
    }
    (directory / "release-manifest.json").write_text(
        json.dumps(manifest, indent=2) + "\n"
    )
    checksums = {
        **manifest["assets"],
        "release-manifest.json": digest(directory / "release-manifest.json"),
    }
    (directory / "SHA256SUMS").write_text(
        "".join(f"{sha}  {name}\n" for name, sha in sorted(checksums.items()))
    )
    return manifest


def verify_bundle(directory=Path("dist")):
    manifest = json.loads((directory / "release-manifest.json").read_text())
    package, _ = project()
    require(manifest["schema"] == 1, "Unknown release manifest schema")
    require(
        manifest["package"] == package["name"]
        and manifest["version"] == package["version"],
        "Wrong package/version",
    )
    require(
        manifest["commit"] == source_commit(), "Artifacts belong to a different commit"
    )
    require(
        manifest["toolchain"] == toolchain(), "Artifacts used a different toolchain"
    )
    require(
        manifest["targets"] == [entry["target"] for entry in targets()],
        "Wrong target matrix",
    )
    expected = expected_assets(package["name"], manifest["version"])
    require(set(manifest["assets"]) == expected, "Incomplete release manifest")
    require(
        {p.name for p in directory.iterdir()}
        == expected | {"release-manifest.json", "SHA256SUMS"},
        "Unexpected files",
    )
    for name, sha in manifest["assets"].items():
        require(digest(directory / name) == sha, f"Checksum mismatch: {name}")
    checksums = {
        **manifest["assets"],
        "release-manifest.json": digest(directory / "release-manifest.json"),
    }
    expected_sums = "".join(
        f"{sha}  {name}\n" for name, sha in sorted(checksums.items())
    )
    require(
        (directory / "SHA256SUMS").read_text() == expected_sums,
        "Checksum file mismatch",
    )
    return manifest


def json_request(url, token=None, missing_ok=False):
    headers = {
        "User-Agent": "aics-release (https://github.com/mevanlc/aics)",
        "Accept": "application/json",
    }
    if token:
        headers["Authorization"] = f"Bearer {token}"
    for attempt in range(4):
        try:
            with urllib.request.urlopen(
                urllib.request.Request(url, headers=headers), timeout=30
            ) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            error.close()
            if missing_ok and error.code == 404:
                return None
            if error.code not in (429, 500, 502, 503, 504) or attempt == 3:
                raise
            time.sleep(2**attempt)
    raise RuntimeError("Unreachable")


def repository():
    repo = os.environ["GITHUB_REPOSITORY"]
    require(
        re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repo), "Invalid repository"
    )
    return repo


def github(path, missing_ok=False):
    return json_request(
        f"https://api.github.com/repos/{repository()}/{path}",
        token=os.environ["GH_TOKEN"],
        missing_ok=missing_ok,
    )


def assert_publication():
    require(
        os.environ.get("GITHUB_ACTIONS") == "true",
        "Publication runs only in GitHub Actions",
    )
    require(os.environ.get("GITHUB_EVENT_NAME") == "push", "Manual runs cannot publish")
    tag = os.environ.get("GITHUB_REF_NAME", "")
    release_version = tag_version(tag)
    require(
        os.environ.get("GITHUB_REF") == f"refs/tags/{tag}",
        "Publication requires a tag push",
    )
    package, _ = project()
    require(
        package["version"] == release_version, "Package version differs from the tag"
    )
    obj = github(f"git/ref/tags/{tag}")["object"]
    while obj["type"] == "tag":
        obj = github(f"git/tags/{obj['sha']}")["object"]
    require(
        obj["type"] == "commit" and obj["sha"] == source_commit(),
        "Release tag moved or checkout differs",
    )
    return tag


def release_state(tag):
    published = github(f"releases/tags/{tag}", missing_ok=True)
    if published is not None:
        return published
    # The by-tag REST endpoint finds published releases, not pending draft tags.
    page = 1
    while True:
        releases = github(f"releases?per_page=100&page={page}")
        for release in releases:
            if release["tag_name"] == tag:
                return release
        if len(releases) < 100:
            return None
        page += 1


def verify_remote_assets(release, directory, complete=False):
    local = {p.name: digest(p) for p in directory.iterdir()}
    remote = release["assets"]
    names = [asset["name"] for asset in remote]
    require(len(set(names)) == len(names), "Duplicate remote release assets")
    require(set(names) <= set(local), "Release contains unexpected assets")
    if complete:
        require(set(names) == set(local), "Published release is incomplete")
    for asset in remote:
        if asset.get("digest"):
            require(
                asset["digest"] == f"sha256:{local[asset['name']]}",
                f"Existing asset differs: {asset['name']}; preserve it and resume the original run",
            )
            continue
        # Older assets may not have GitHub's SHA-256 digest field.
        with tempfile.TemporaryDirectory() as temporary:
            run(
                "gh",
                "release",
                "download",
                release["tag_name"],
                "--repo",
                repository(),
                "--pattern",
                asset["name"],
                "--dir",
                temporary,
            )
            require(
                digest(Path(temporary) / asset["name"]) == local[asset["name"]],
                f"Existing asset differs: {asset['name']}; preserve it and resume the original run",
            )
    return set(local) - set(names)


def stage(directory=Path("dist")):
    manifest = verify_bundle(directory)
    tag = assert_publication()
    release = release_state(tag)
    if release is None:
        notes = (
            f"Install from crates.io: cargo install {manifest['package']} --version {manifest['version']} --locked\n\n"
            "Or download the archive for your platform. SHA256SUMS covers all downloadable assets; "
            "release-manifest.json records the source commit and build toolchain.\n"
        )
        with tempfile.TemporaryDirectory() as temporary:
            notes_path = Path(temporary) / "notes.md"
            notes_path.write_text(notes)
            args = [
                "gh",
                "release",
                "create",
                tag,
                "--repo",
                repository(),
                "--draft",
                "--verify-tag",
                "--title",
                tag,
                "--generate-notes",
                "--notes-file",
                str(notes_path),
            ]
            if "-" in manifest["version"]:
                args.append("--prerelease")
            run(*args)
        # Creation can succeed before the draft appears in GitHub's read APIs.
        # Retry reads only: never create another draft to recover visibility.
        for attempt in range(6):
            release = release_state(tag)
            if release is not None:
                break
            if attempt < 5:
                time.sleep(2**attempt)
        require(
            release is not None,
            "Created draft is not visible yet; rerun failed jobs to reconcile",
        )
    require(
        release["prerelease"] == ("-" in manifest["version"]),
        "Existing release has the wrong prerelease status",
    )
    missing = verify_remote_assets(release, directory, complete=not release["draft"])
    if missing:
        run(
            "gh",
            "release",
            "upload",
            tag,
            "--repo",
            repository(),
            *(str(directory / name) for name in sorted(missing)),
        )
    verify_remote_assets(release_state(tag), directory, complete=True)


def crate_checksum(package, release_version):
    data = json_request(
        f"https://crates.io/api/v1/crates/{package}/{release_version}", missing_ok=True
    )
    return data["version"]["checksum"] if data else None


def check_registry(manifest):
    expected = manifest["assets"][f"{manifest['package']}-{manifest['version']}.crate"]
    actual = crate_checksum(manifest["package"], manifest["version"])
    if actual is None:
        return False
    require(
        actual == expected, "crates.io already has different bytes for this version"
    )
    return True


def check_package(directory=Path("dist")):
    manifest = verify_bundle(directory)
    run("cargo", "package", "--locked", "--allow-dirty", "--no-verify")
    filename = f"{manifest['package']}-{manifest['version']}.crate"
    require(
        digest(Path("target/package") / filename) == manifest["assets"][filename],
        "Recreated crate differs from the verified CI package",
    )


def publish_crate(directory=Path("dist")):
    manifest = verify_bundle(directory)
    tag = assert_publication()
    release = release_state(tag)
    require(release is not None, "Stage the draft before publishing the crate")
    verify_remote_assets(release, directory, complete=True)
    if check_registry(manifest):
        print("Matching crate is already published")
        return
    # CI compiled the extracted crate; check-package reproduced its exact bytes.
    filename = f"{manifest['package']}-{manifest['version']}.crate"
    require(
        digest(Path("target/package") / filename) == manifest["assets"][filename],
        "Run check-package first",
    )
    result = subprocess.run(
        ["cargo", "publish", "--locked", "--allow-dirty", "--no-verify"], check=False
    )
    # Upload can succeed even if Cargo subsequently times out waiting for indexing.
    for attempt in range(30):
        if check_registry(manifest):
            print("Published crate checksum verified")
            return
        if attempt < 29:
            time.sleep(10)
    raise RuntimeError(
        f"Crate is not visible after publication (cargo exit {result.returncode}); rerun to reconcile"
    )


def finalize(directory=Path("dist")):
    manifest = verify_bundle(directory)
    tag = assert_publication()
    require(
        check_registry(manifest), "Crate must be published before the GitHub release"
    )
    release = release_state(tag)
    require(release is not None, "Missing staged release")
    verify_remote_assets(release, directory, complete=True)
    if release["draft"]:
        args = ["gh", "release", "edit", tag, "--repo", repository(), "--draft=false"]
        if "-" in manifest["version"]:
            args.extend(["--prerelease", "--latest=false"])
        run(*args)
    else:
        print("Matching GitHub release is already published")
    final = release_state(tag)
    require(
        not final["draft"] and final.get("immutable"),
        "Release must be published and immutable",
    )
    print(final["html_url"])


def copy_crate(destination=Path("dist")):
    package, _ = project()
    source = Path("target/package") / f"{package['name']}-{package['version']}.crate"
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination / source.name)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    for command in (
        "version",
        "tag",
        "stamp",
        "archive",
        "smoke",
        "smoke-search",
        "verify-binaries",
    ):
        commands.add_parser(command).add_argument("value")
    for command in (
        "matrix",
        "toolchain",
        "bundle",
        "verify",
        "stage",
        "check-package",
        "publish-crate",
        "finalize",
        "copy-crate",
        "crate-published",
    ):
        commands.add_parser(command)
    args = parser.parse_args()
    match args.command:
        case "version":
            print(version(args.value))
        case "tag":
            print(tag_version(args.value))
        case "stamp":
            stamp(args.value)
        case "matrix":
            print(json.dumps({"include": targets()}, separators=(",", ":")))
        case "toolchain":
            print(toolchain())
        case "archive":
            print(package_binary(args.value))
        case "smoke":
            smoke(args.value)
        case "smoke-search":
            smoke_search(Path(args.value))
        case "verify-binaries":
            verify_binaries(Path(args.value), "")
        case "bundle":
            bundle()
        case "verify":
            verify_bundle()
        case "stage":
            stage()
        case "check-package":
            check_package()
        case "publish-crate":
            publish_crate()
        case "finalize":
            finalize()
        case "copy-crate":
            copy_crate()
        case "crate-published":
            print(str(check_registry(verify_bundle())).lower())


if __name__ == "__main__":
    try:
        main()
    except (
        ValueError,
        KeyError,
        OSError,
        RuntimeError,
        subprocess.CalledProcessError,
    ) as error:
        sys.exit(f"release: {error}")
