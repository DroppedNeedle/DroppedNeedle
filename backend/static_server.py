import mimetypes
from pathlib import Path
from fastapi import FastAPI, HTTPException
from fastapi.responses import FileResponse
from fastapi.staticfiles import StaticFiles


# slim base images omit woff2 from mime.types; register it so fonts serve as font/woff2
mimetypes.add_type("font/woff2", ".woff2")
mimetypes.add_type("font/woff", ".woff")

_NO_CACHE_HEADERS = {"Cache-Control": "no-cache"}
_IMMUTABLE_CACHE_CONTROL = "public, max-age=31536000, immutable"


class FrontendStaticFiles(StaticFiles):
    async def get_response(self, path: str, scope: dict):
        response = await super().get_response(path, scope)
        if path.startswith("immutable/") and response.status_code in (200, 304):
            response.headers["Cache-Control"] = _IMMUTABLE_CACHE_CONTROL
        return response


def mount_frontend(app: FastAPI):
    backend_static = Path(__file__).parent / "static"
    frontend_root = Path(__file__).resolve().parents[1] / "frontend"
    build_candidates = [backend_static, frontend_root / "build"]

    def first_existing_build() -> Path:
        for candidate in build_candidates:
            if (candidate / "index.html").exists():
                return candidate
        return backend_static

    build_dir = first_existing_build()
    index_html = build_dir / "index.html"
    asset_dirs = [build_dir, frontend_root / "static"]

    def resolve_asset(filename: str) -> Path | None:
        for directory in asset_dirs:
            candidate = directory / filename
            if candidate.exists():
                return candidate
        return None

    if (build_dir / "_app").exists():
        app.mount("/_app", FrontendStaticFiles(directory=build_dir / "_app", html=False), name="_app")

    if (img_dir := build_dir / "img").exists():
        app.mount("/img", StaticFiles(directory=img_dir, html=False), name="img")

    if (fonts_dir := build_dir / "fonts").exists():
        app.mount("/fonts", StaticFiles(directory=fonts_dir, html=False), name="fonts")

    @app.get("/robots.txt")
    async def serve_robots():
        if robots := resolve_asset("robots.txt"):
            return FileResponse(robots, media_type="text/plain", headers={"Cache-Control": "public, max-age=86400"})
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/logo.png")
    async def serve_logo():
        if logo := resolve_asset("logo.png"):
            return FileResponse(logo)
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/logo_wide.png")
    async def serve_logo_wide():
        if logo := resolve_asset("logo_wide.png"):
            return FileResponse(logo)
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/logo_wide_white.png")
    async def serve_logo_wide_white():
        if logo := resolve_asset("logo_wide_white.png"):
            return FileResponse(logo)
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/logo_icon.png")
    async def serve_logo_icon():
        if logo := resolve_asset("logo_icon.png"):
            return FileResponse(logo)
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/favicon.ico")
    async def serve_favicon_ico():
        if icon := resolve_asset("favicon.ico"):
            return FileResponse(icon, media_type="image/x-icon", headers={"Cache-Control": "public, max-age=604800"})
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/favicon-{size}.png")
    async def serve_favicon_png(size: str):
        if icon := resolve_asset(f"favicon-{size}.png"):
            return FileResponse(icon, media_type="image/png", headers={"Cache-Control": "public, max-age=604800"})
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/apple-touch-icon.png")
    async def serve_apple_touch_icon():
        if icon := resolve_asset("apple-touch-icon.png"):
            return FileResponse(icon, media_type="image/png", headers={"Cache-Control": "public, max-age=604800"})
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/android-chrome-{size}.png")
    async def serve_android_chrome(size: str):
        if icon := resolve_asset(f"android-chrome-{size}.png"):
            return FileResponse(icon, media_type="image/png", headers={"Cache-Control": "public, max-age=604800"})
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/site.webmanifest")
    async def serve_webmanifest():
        if manifest := resolve_asset("site.webmanifest"):
            return FileResponse(manifest, media_type="application/manifest+json", headers={"Cache-Control": "public, max-age=604800"})
        raise HTTPException(status_code=404, detail="Not found")

    @app.get("/")
    async def serve_root():
        if index_html.exists():
            return FileResponse(index_html, headers=_NO_CACHE_HEADERS)
        raise HTTPException(status_code=404, detail="Frontend not built yet")

    @app.get("/{full_path:path}")
    async def serve_spa_routes(full_path: str):
        if full_path.startswith("api"):
            raise HTTPException(status_code=404, detail="API route not found")
        if index_html.exists():
            return FileResponse(index_html, headers=_NO_CACHE_HEADERS)
        raise HTTPException(status_code=404, detail="Frontend not built yet")
