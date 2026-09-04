"""Unsupported entrypoint guard (F-NL-03 clean cutover).

The legacy ``main:app`` composition was removed: it served the retired
``LibraryScanner`` surface that GH #290 reported against. Launch the supported
target application instead:

    uvicorn target_main:app --host 0.0.0.0 --port 8688

See CONTRIBUTING.md for local development and the Docker image supervisor
launch path for ``target_main:app``.
"""

raise SystemExit(
    "Unsupported installation: main:app was removed in the F-NL-03 scan "
    "cutover. Launch target_main:app instead - see CONTRIBUTING.md for local "
    "development and the Docker image supervisor launch path."
)
