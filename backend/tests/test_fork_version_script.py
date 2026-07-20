from pathlib import Path
import subprocess


SCRIPT = Path(__file__).parents[2] / ".github/scripts/next-fork-version.sh"


def _git(repository: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
    )
    return result.stdout.strip()


def _commit(repository: Path, message: str) -> None:
    marker = repository / "marker"
    marker.write_text(f"{message}\n", encoding="utf-8")
    _git(repository, "add", "marker")
    _git(repository, "commit", "-m", message)


def _next_version(repository: Path) -> str:
    return subprocess.run(
        ["bash", str(SCRIPT)],
        cwd=repository,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()


def test_fork_version_sequence_and_upstream_reset(tmp_path: Path) -> None:
    repository = tmp_path / "repo"
    repository.mkdir()
    _git(repository, "init")
    _git(repository, "config", "user.name", "Test")
    _git(repository, "config", "user.email", "test@example.com")

    _commit(repository, "upstream 2.3.0")
    _git(repository, "tag", "v2.3.0")
    _git(repository, "tag", "v2.3.0-fork.1")
    _commit(repository, "upstream prerelease")
    _git(repository, "tag", "v2.4.0rc1")
    _git(repository, "tag", "v2.3.0-beta.1")
    _commit(repository, "fork one")
    assert _next_version(repository) == "v2.3.0.post1"

    _git(repository, "tag", "v2.3.0.post1")
    assert _next_version(repository) == ""
    _commit(repository, "fork two")
    assert _next_version(repository) == "v2.3.0.post2"

    _commit(repository, "upstream 2.4.0")
    _git(repository, "tag", "v2.4.0")
    _commit(repository, "fork on new base")
    assert _next_version(repository) == "v2.4.0.post1"
