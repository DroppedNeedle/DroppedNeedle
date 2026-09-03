import asyncio
import logging
import os
import threading
from pathlib import Path
from typing import Any

import aiofiles
import aiofiles.os
import msgspec

logger = logging.getLogger(__name__)


def _fsync_file_contents(path: Path) -> None:
    with open(path, "rb") as handle:
        os.fsync(handle.fileno())


def _fsync_directory_best_effort(directory: Path) -> None:
    try:
        descriptor = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    except OSError:
        return
    try:
        os.fsync(descriptor)
    except OSError:
        pass
    finally:
        try:
            os.close(descriptor)
        except OSError:
            pass


_file_locks: dict[str, threading.Lock] = {}
_locks_lock = threading.Lock()
_async_locks: dict[str, asyncio.Lock] = {}


def _get_file_lock(file_path: Path) -> threading.Lock:
    path_str = str(file_path.resolve())
    with _locks_lock:
        if path_str not in _file_locks:
            _file_locks[path_str] = threading.Lock()
        return _file_locks[path_str]


def _get_async_lock(file_path: Path) -> asyncio.Lock:
    path_str = str(file_path.resolve())
    if path_str not in _async_locks:
        _async_locks[path_str] = asyncio.Lock()
    return _async_locks[path_str]


def atomic_write_json(file_path: Path, data: Any, indent: int = 2) -> None:
    lock = _get_file_lock(file_path)
    
    with lock:
        file_path.parent.mkdir(parents=True, exist_ok=True)
        tmp_path = file_path.with_suffix(file_path.suffix + '.tmp')
        
        try:
            _ = indent
            with open(tmp_path, 'wb') as f:
                f.write(msgspec.json.encode(data))
                f.flush()
                os.fsync(f.fileno())

            tmp_path.replace(file_path)
            _fsync_directory_best_effort(file_path.parent)
            logger.debug(f"Atomically wrote JSON to {file_path}")
            
        except Exception as e:
            if tmp_path.exists():
                try:
                    tmp_path.unlink()
                except OSError as cleanup_error:
                    logger.warning("Couldn't remove temp file %s: %s", tmp_path, cleanup_error)
            logger.error(f"Failed to write JSON to {file_path}: {e}")
            raise


async def atomic_write_json_async(file_path: Path, data: Any, indent: int = 2) -> None:
    lock = _get_async_lock(file_path)
    
    async with lock:
        await aiofiles.os.makedirs(file_path.parent, exist_ok=True)
        tmp_path = file_path.with_suffix(file_path.suffix + '.tmp')
        
        try:
            _ = indent
            content = msgspec.json.encode(data)
            async with aiofiles.open(tmp_path, 'wb') as f:
                await f.write(content)
                await f.flush()
            await asyncio.to_thread(_fsync_file_contents, tmp_path)

            await asyncio.to_thread(tmp_path.replace, file_path)
            await asyncio.to_thread(_fsync_directory_best_effort, file_path.parent)
            logger.debug(f"Atomically wrote JSON to {file_path}")
            
        except Exception as e:
            try:
                if tmp_path.exists():
                    await aiofiles.os.remove(tmp_path)
            except OSError as cleanup_error:
                logger.warning("Couldn't remove temp file %s: %s", tmp_path, cleanup_error)
            logger.error(f"Failed to write JSON to {file_path}: {e}")
            raise


def read_json(file_path: Path, default: Any = None) -> Any:
    lock = _get_file_lock(file_path)
    
    with lock:
        if not file_path.exists():
            return default
        
        with open(file_path, 'rb') as f:
            return msgspec.json.decode(f.read())


async def read_json_async(file_path: Path, default: Any = None) -> Any:
    lock = _get_async_lock(file_path)
    
    async with lock:
        if not file_path.exists():
            return default
        
        async with aiofiles.open(file_path, 'rb') as f:
            content = await f.read()
            return msgspec.json.decode(content)
