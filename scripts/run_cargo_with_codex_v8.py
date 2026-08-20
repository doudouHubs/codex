#!/usr/bin/env python3
"""为需要 Code Mode host 的 Cargo 命令注入 sandbox V8 构建环境。"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys


SCRIPT_ROOT = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_ROOT.parent
CODEX_RS_ROOT = REPO_ROOT / "codex-rs"

# 该脚本从 justfile 调用时 cwd 已经是 codex-rs，但显式固定路径，避免
# 用户从仓库根目录直接调用时 Cargo 找不到 workspace manifest。
sys.path.insert(0, str(SCRIPT_ROOT))

from codex_package.targets import TARGET_SPECS
from codex_package.targets import default_target
from codex_package.v8 import resolve_codex_v8_cargo_env


def main() -> int:
    if len(sys.argv) < 2:
        print(
            "usage: run_cargo_with_codex_v8.py <command> [args...]",
            file=sys.stderr,
        )
        return 2

    target = os.environ.get("TARGET") or default_target()
    try:
        target_spec = TARGET_SPECS[target]
    except KeyError:
        print(
            f"unsupported Rust target for Codex V8 artifacts: {target}", file=sys.stderr
        )
        return 2

    cargo_env = os.environ.copy()
    # 复用打包流程的版本解析、下载和 SHA-256 校验，避免 justfile 重复维护 V8 版本。
    cargo_env.update(resolve_codex_v8_cargo_env(target_spec, environ=cargo_env))
    return subprocess.run(
        sys.argv[1:],
        cwd=CODEX_RS_ROOT,
        env=cargo_env,
        check=False,
    ).returncode


if __name__ == "__main__":
    raise SystemExit(main())
