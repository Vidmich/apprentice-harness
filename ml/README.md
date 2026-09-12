# apprentice-ml

Python workspace for apprentice-harness: dataset builders, trainers,
evaluators and compute backends (from M05/M06). M00 ships only the package
skeleton and a trace reader.

```
uv sync                      # create .venv with dev tools
uv run apprentice-ml --version
uv run apprentice-ml traces stats --db ~/.local/share/apprentice-harness/traces.sqlite
uv run pytest
uv run ruff check .
```
