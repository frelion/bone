# 复验已集成的分页契约

`verify_cli.py` 是运行前冻结的 22 项检查；`verify_public_api.py` 是独立评审者在
实际 API 签名确定后补充的 17 项检查。两者原样保留，无模型请求或真实账号配置。
`public_api_caller.rs` 只调用公共 API。它在临时目录构建，不增加 BONE 的 package。

从仓库根执行（macOS/Linux，Rust 与 Python 3）：

```sh
cargo build --locked
python3 -B - <<'PY'
import json, os, pathlib, shutil, subprocess, tempfile
repo = pathlib.Path.cwd()
checks = repo / 'docs/results/2026-10-01-engineering/verification'
binary = repo / 'target/debug/bone'
with tempfile.TemporaryDirectory(prefix='bone-history-check-') as tmp:
    work = pathlib.Path(tmp)
    (work / 'src').mkdir()
    shutil.copy2(checks / 'public_api_caller.rs', work / 'src/main.rs')
    shutil.copy2(repo / 'Cargo.lock', work / 'Cargo.lock')
    (work / 'Cargo.toml').write_text(
        '[package]\nname="bone-independent-history-caller"\nversion="0.0.0"\nedition="2024"\n'
        '[dependencies]\nbone={path=' + json.dumps(str(repo)) + '}\nserde_json="1"\n')
    env = {**os.environ, 'CARGO_TARGET_DIR': str(repo / 'target')}
    subprocess.run(['cargo', 'build', '--offline', '--manifest-path', str(work / 'Cargo.toml')], env=env, check=True)
    caller = repo / 'target/debug/bone-independent-history-caller'
    subprocess.run(['python3', '-B', str(checks / 'verify_cli.py'), '--binary', str(binary), '--output', str(work / 'cli.json')], check=True)
    subprocess.run(['python3', '-B', str(checks / 'verify_public_api.py'), '--caller', str(caller), '--binary', str(binary), '--output', str(work / 'api.json')], check=True)
PY
```

检查结果输出到终端；临时数据库和结果文件随后删除。需要留存时改用固定输出目录。
冻结契约见 [contract.md](contract.md)。这验证分页契约，不验证模型能否自主完成任务。
