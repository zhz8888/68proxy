#!/usr/bin/env python3
"""项目构建缓存清理脚本。

先遍历项目内的可清理目录与文件并打印清单，经用户确认（Y/n）后再执行删除。
用法：python3 tools/clean_cache.py [--root DIR]
"""

from __future__ import annotations

import argparse
import os
import shutil
import sys
from pathlib import Path

# 命中即整体删除的目录名，命中后不再向下遍历
CACHE_DIR_NAMES = {
    "node_modules",
    "target",
    "dist",
    "dist-ssr",
    ".vite",
    ".turbo",
    ".cache",
    ".parcel-cache",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
}

# 命中即删除的文件名与后缀
CACHE_FILE_NAMES = {".DS_Store"}
CACHE_FILE_SUFFIXES = (".log", ".tsbuildinfo")

# 按相对路径精确匹配的生成目录，避免误伤同名目录
CACHE_DIR_RELPATHS = {"src-tauri/gen/schemas"}

# 遍历时直接跳过、绝不进入的目录
SKIP_DIR_NAMES = {".git"}

# 清单中的用途说明
DIR_LABELS = {
    "node_modules": "依赖目录",
    "target": "Rust 构建产物",
    "dist": "前端构建产物",
    "dist-ssr": "前端构建产物",
    ".vite": "Vite 缓存",
    ".turbo": "Turbo 缓存",
    ".cache": "通用缓存",
    ".parcel-cache": "Parcel 缓存",
    "__pycache__": "Python 字节码缓存",
    ".pytest_cache": "Pytest 缓存",
    ".mypy_cache": "Mypy 缓存",
    ".ruff_cache": "Ruff 缓存",
}


def is_cache_dir(name: str) -> bool:
    """判断目录名是否属于可清理的缓存目录。"""
    return name in CACHE_DIR_NAMES or name.endswith(".egg-info")


def is_cache_file(name: str) -> bool:
    """判断文件名是否属于可清理的缓存文件。"""
    return name in CACHE_FILE_NAMES or name.endswith(CACHE_FILE_SUFFIXES)


def describe(path: Path) -> str:
    """给出条目的简短用途说明。"""
    if path.as_posix().endswith("gen/schemas"):
        return "Tauri 生成的 schema"
    if path.is_dir() and not path.is_symlink():
        return DIR_LABELS.get(path.name, "生成目录")
    return "缓存文件"


def add_file_size(stat_result, seen: set) -> int:
    """累加单个文件大小；硬链接按 inode 只计一次，避免重复计数。"""
    if stat_result.st_nlink > 1:
        key = (stat_result.st_dev, stat_result.st_ino)
        if key in seen:
            return 0
        seen.add(key)
    return stat_result.st_size


def dir_size(path: Path, seen: set) -> int:
    """递归统计目录占用字节数，跳过无法读取的条目。"""
    total = 0
    stack = [path]
    while stack:
        try:
            with os.scandir(stack.pop()) as entries:
                for entry in entries:
                    try:
                        if entry.is_dir(follow_symlinks=False):
                            stack.append(Path(entry.path))
                        elif entry.is_file(follow_symlinks=False):
                            total += add_file_size(entry.stat(follow_symlinks=False), seen)
                    except OSError:
                        continue
        except OSError:
            continue
    return total


def entry_size(path: Path) -> int:
    """统计单个条目的字节数，`seen` 用于硬链接去重。"""
    try:
        if path.is_symlink():
            return 0
        # 每个条目独立去重：硬链接主要出现在单个目录内部（如 Rust target）
        seen: set = set()
        if path.is_dir():
            return dir_size(path, seen)
        return add_file_size(path.stat(), seen)
    except OSError:
        return 0


def human(size: int) -> str:
    """把字节数格式化为可读大小。"""
    value = float(size)
    for unit in ("B", "KB", "MB", "GB", "TB"):
        if value < 1024 or unit == "TB":
            return f"{value:.1f} {unit}"
        value /= 1024
    return f"{value:.1f} TB"


def scan(root: Path) -> tuple[list[Path], list[Path]]:
    """遍历项目，返回可清理的目录与文件清单。命中目录后不再深入。"""
    dirs: list[Path] = []
    files: list[Path] = []
    for dirpath, dirnames, filenames in os.walk(root):
        current = Path(dirpath)
        rel_dir = current.relative_to(root)
        kept = []
        for name in dirnames:
            if name in SKIP_DIR_NAMES:
                continue
            rel = (rel_dir / name).as_posix()
            if is_cache_dir(name) or rel in CACHE_DIR_RELPATHS:
                dirs.append(current / name)
                continue
            kept.append(name)
        dirnames[:] = kept
        for name in filenames:
            if is_cache_file(name):
                files.append(current / name)
    return dirs, files


def _on_rm_error(func, path, exc) -> None:
    """删除失败时先补权限再重试一次（处理只读文件）。"""
    try:
        os.chmod(path, 0o700)
        func(path)
    except OSError:
        pass


def _rmtree(path: Path) -> None:
    """跨版本调用 shutil.rmtree 并传入错误回调。"""
    if sys.version_info >= (3, 12):
        shutil.rmtree(path, onexc=_on_rm_error)
    else:  # 兼容 3.12 以下版本的 onerror 参数
        shutil.rmtree(path, onerror=lambda f, p, i: _on_rm_error(f, p, i[1]))


def remove(path: Path) -> str | None:
    """删除单个条目，返回错误信息；成功时返回 None。"""
    try:
        if path.is_symlink() or not path.is_dir():
            path.unlink()
        else:
            _rmtree(path)
    except OSError as exc:
        return str(exc)
    return None


def confirm(total_size: int, count: int) -> bool:
    """询问用户是否执行清理，默认回车即同意。"""
    prompt = f"\n确认清理以上 {count} 项（约 {human(total_size)}）？[Y/n] "
    while True:
        try:
            answer = input(prompt).strip().lower()
        except EOFError:  # 非交互环境默认放弃，避免误删
            print()
            return False
        if answer in ("", "y", "yes"):
            return True
        if answer in ("n", "no"):
            return False
        print("请输入 Y 或 n。")


def main() -> int:
    parser = argparse.ArgumentParser(description="清理项目内的构建缓存与生成产物")
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="项目根目录，默认取脚本所在仓库的根目录",
    )
    args = parser.parse_args()
    root = args.root.resolve()
    if not root.is_dir():
        print(f"错误：根目录不存在 {root}", file=sys.stderr)
        return 1

    print(f"项目根目录：{root}\n正在遍历，请稍候……")
    dirs, files = scan(root)

    entries = dirs + files
    if not entries:
        print("未发现可清理的目录或文件。")
        return 0

    # 计算尺寸并按路径排序，便于阅读
    sized = sorted(((entry_size(p), p) for p in entries), key=lambda item: item[1].as_posix())
    total_size = sum(size for size, _ in sized)

    print(f"\n可清理目录（{len(dirs)}）：")
    for size, path in sized:
        if path in dirs:
            print(f"  {human(size):>10}  {path.relative_to(root).as_posix()}/  [{describe(path)}]")
    print(f"\n可清理文件（{len(files)}）：")
    for size, path in sized:
        if path in files:
            print(f"  {human(size):>10}  {path.relative_to(root).as_posix()}  [{describe(path)}]")
    print(f"\n合计：{len(entries)} 项，约 {human(total_size)}")
    print("提示：清理后需重新执行 pnpm install / cargo build 才能恢复构建环境。")

    if not confirm(total_size, len(entries)):
        print("已取消，未做任何删除。")
        return 0

    print()
    freed = 0
    failures: list[tuple[Path, str]] = []
    for size, path in sized:
        # 安全兜底：目标必须位于项目根目录内
        if not path.is_relative_to(root) or path == root:
            failures.append((path, "路径不在项目根目录内"))
            continue
        error = remove(path)
        if error is None:
            freed += size
            print(f"  已删除  {path.relative_to(root).as_posix()}  ({human(size)})")
        else:
            failures.append((path, error))

    print(f"\n清理完成：释放约 {human(freed)}，成功 {len(entries) - len(failures)} 项。")
    for path, error in failures:
        print(f"  失败    {path}  ({error})", file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
