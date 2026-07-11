"""DroppedNeedle acquisition plugins.

The acquisition layer (search for release candidates, enqueue downloads, track
progress, locate completed files, cancel) is plugin-driven. Today's Soulseek
(slskd) and Usenet (SABnzbd) support are the first two built-in plugins; they
live in ``plugins/builtin`` and wrap the existing client code unchanged.

Third-party plugins are single ``.py`` modules or packages dropped into
``{ROOT_APP_DIR}/plugins``, or installed packages exposing the
``droppedneedle.plugins`` entry-point group. See ``docs/plugins/`` for the
full authoring guide.

The IMPORT pipeline (post-download identification / move / registration) is
core and NOT pluggable - a plugin's job ends at "the finished files are here".
"""

from plugins.base import (  # noqa: F401
    API_VERSION,
    AcquisitionPlugin,
    Candidate,
    PLUGIN_SECRET_MASK,
    SearchRequest,
    SelectOption,
    SettingsField,
    TestResult,
)
from plugins.manager import PluginManager, PluginRecord  # noqa: F401
