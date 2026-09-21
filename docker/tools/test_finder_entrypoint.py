from pathlib import Path


ENTRYPOINT = Path(__file__).parents[1] / "entrypoint.sh"


def test_finder_browser_uses_absolute_path_after_su_resets_path() -> None:
    script = ENTRYPOINT.read_text(encoding="utf-8")

    assert "FINDER_BROWSER_PROFILE=$FINDER_BROWSER_PROFILE /opt/tools/finder-browser" in script
