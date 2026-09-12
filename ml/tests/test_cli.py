from apprentice_ml import __version__
from apprentice_ml.cli import build_parser


def test_version_string() -> None:
    assert __version__.count(".") == 2


def test_parser_builds() -> None:
    parser = build_parser()
    assert parser.prog == "apprentice-ml"
