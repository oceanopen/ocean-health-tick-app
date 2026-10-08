#!/usr/bin/env python3
"""创建或复用 Gitee Release（幂等），输出 release_id 供下游步骤守卫使用。

由 .github/workflows/release-gitee-sync.yml 在镜像 tag 到 Gitee 之后调用。
失败语义：真异常（网络/鉴权/Gitee 错误/bug/超时）直接抛出 → exit 非 0 → 步骤红，绝不静默吞掉。
仅「良性幂等条件」优雅处理：tag 已镜像但无 release（Gitee 返回 200+null 或 404）→ 进入创建。
实时日志：stdout 行缓冲，每步 print 立即刷到 Actions 日志。

输入（环境变量）：
  GITEE_OWNER   Gitee 组织/用户名（ocean-open）
  GITEE_REPO    Gitee 仓库名（ocean-health-tick-app）
  TAG           发版 tag（如 v0.1.38）
  GITEE_TOKEN   Gitee 个人访问令牌
  GITHUB_SHA    tag 实际指向的 commit（target_commitish）
  GITHUB_OUTPUT Actions 步骤输出文件路径

输出：
  GITHUB_OUTPUT 追加 `release_id=<id>`（仅成功时）
"""
import json
import os
import sys
import urllib.error
import urllib.request

# 行缓冲：每行 print 立即 flush 到 Actions 日志，实时可见
sys.stdout.reconfigure(line_buffering=True)

API_TIMEOUT = 60  # Gitee API 超时（秒）


def _read_json(resp):
    """读取响应体并解析 JSON。空 body / 非 JSON（如 Gitee 偶发的 HTML 错误页）→ 返回 None。"""
    raw = resp.read().decode("utf-8", errors="replace")
    if not raw.strip():
        return None
    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        return None


def main() -> None:
    owner = os.environ["GITEE_OWNER"]
    repo = os.environ["GITEE_REPO"]
    tag = os.environ["TAG"]
    token = os.environ["GITEE_TOKEN"]
    sha = os.environ.get("GITHUB_SHA", "main")
    base = f"https://gitee.com/api/v5/repos/{owner}/{repo}"
    print(f"Create/get Gitee Release for tag {tag}")

    # 幂等：先查 tag 是否已有 release
    # 注意 Gitee 怪癖：tag 已镜像但尚无 Release 时，GET /releases/tags/{tag} 返回 200 + null（非 404）。
    # 故不能盲下标 ["id"] —— 只有「是 dict 且含 id」才算已有 release，否则进入创建分支。
    print("  querying existing release…")
    try:
        with urllib.request.urlopen(
            f"{base}/releases/tags/{tag}?access_token={token}", timeout=API_TIMEOUT
        ) as r:
            data = _read_json(r)
    except urllib.error.HTTPError as e:
        if e.code != 404:
            raise
        data = None

    rid = ""
    if isinstance(data, dict) and data.get("id") is not None:
        rid = str(data["id"])
        print(f"  existing release found: id={rid}")
    else:
        print(f"  no existing release (query returned: {data!r:.60})")

    if not rid:
        print("  creating release…")
        # Gitee v5 鉴权坑：access_token 放 body，放 header 会返回 HTML 错误页
        body = json.dumps({
            "access_token": token,
            "tag_name": tag,
            "name": f"Ocean Health Tick {tag}",
            "body": f"Release {tag} (mirrored from GitHub)",
            "target_commitish": sha,
        }).encode()
        req = urllib.request.Request(
            f"{base}/releases",
            data=body,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=API_TIMEOUT) as r:
            data = _read_json(r)
        if not isinstance(data, dict) or data.get("id") is None:
            raise RuntimeError(f"Gitee create-release returned unexpected response: {data!r:.200}")
        rid = str(data["id"])
        print(f"  created release: id={rid}")

    with open(os.environ["GITHUB_OUTPUT"], "a") as f:
        f.write(f"release_id={rid}\n")
    print(f"release_id={rid}")


if __name__ == "__main__":
    # 真异常直接抛出 → Python 打印 traceback + exit 非 0 → 步骤红，绝不静默吞掉
    main()
