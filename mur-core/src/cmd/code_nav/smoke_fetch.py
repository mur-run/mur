# Run by `mur code-nav setup` with serena's own interpreter (#1993).
#
# Has serena fetch each language server it downloads itself, exactly as it
# would on first start (same SERENA_HOME, same code path), and prints one JSON
# line: {"<lang>": {"ok": [argv...]} | {"err": "<why>"}}. The argv is the one
# serena would launch, so setup can smoke it under the agent's seal.
#
# argv: <root> <lang>...   root is a scratch dir standing in for the project.
import json
import sys

from serena.config.serena_config import SerenaPaths
from solidlsp.ls import SolidLanguageServer
from solidlsp.ls_config import LanguageServerConfig, LanguageServerId
from solidlsp.settings import SolidLSPSettings

root = sys.argv[1]
out = {}
for name in sys.argv[2:]:
    try:
        settings = SolidLSPSettings(solidlsp_dir=SerenaPaths().serena_user_home_dir)
        config = LanguageServerConfig(ls_id=LanguageServerId(name))
        ls = SolidLanguageServer.create(config, root, solidlsp_settings=settings)
        cmd = ls._get_process_launch_info().cmd
        out[name] = {"ok": cmd if isinstance(cmd, list) else [cmd]}
    except Exception as e:  # report every language, never stop at the first
        out[name] = {"err": f"{type(e).__name__}: {e}"}
print(json.dumps(out))
