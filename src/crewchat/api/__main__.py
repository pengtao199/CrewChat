"""`uv run crewchat` — 启动后端并在浏览器打开界面。

后续做 Mac App 时，这里换成 pywebview 原生窗口即可，下面的服务逻辑不变。
"""

from __future__ import annotations

import threading
import time
import webbrowser

import uvicorn

from ..config import get_settings


def main() -> None:
    settings = get_settings()
    settings.ensure_dirs()
    host, port = settings.host, settings.port
    url = f"http://{host}:{port}/"

    if "--no-browser" not in __import__("sys").argv:
        threading.Thread(target=_open_when_ready, args=(url,), daemon=True).start()

    uvicorn.run("crewchat.api:app", host=host, port=port, log_level="info")


def _open_when_ready(url: str, attempts: int = 40) -> None:
    import urllib.error
    import urllib.request

    for _ in range(attempts):
        try:
            urllib.request.urlopen(url, timeout=1)
            webbrowser.open(url)
            return
        except (urllib.error.URLError, OSError):
            time.sleep(0.5)


if __name__ == "__main__":
    main()
