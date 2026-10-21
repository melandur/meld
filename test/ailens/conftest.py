import pytest

from test.ailens.fixture_repo import FixtureRepo


@pytest.fixture
def fixture_repo(tmp_path):
    """An empty git repo on ``main``, isolated from the user's git config"""
    return FixtureRepo(tmp_path / "repo")
