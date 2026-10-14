#!/usr/bin/env python3
"""离线 A2.10A 原版 Stage / 实际 factory-adapter pipeline 的有界入口。
目标设备只需 stage 同 hash bundle 并直接运行 Cargo JSON 给出的 test ELF。
本工具没有网络、设备、凭据、Git 或生产 source 写入。
"""
import argparse
import ast
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[2]
RUST = Path('/Users/sun/.rustup/toolchains/1.96.0-aarch64-apple-darwin/bin')
PREFIX = 'active_session::tests::legacy_pipeline::'
OUT = ROOT / 'target/pipeline-evidence-rust196'
DATA = ROOT / 'target/pipeline-workloads-v5'
OWNED = [ROOT / 'crates/frd-protocol-rdp/src/legacy_pipeline.rs', ROOT / 'crates/frd-protocol-rdp/src/active_session.rs', ROOT / 'tests/legacy-oracle/src/bin/pipeline.rs', ROOT / 'tests/legacy-oracle/Cargo.toml', Path(__file__).resolve()]

def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()

def environment(target, observe=False):
    env = os.environ.copy()
    for key in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'FRD_PIPELINE_STAGE_REQUEST'):
        env.pop(key, None)
    env.update(PATH=str(RUST) + os.pathsep + env.get('PATH', ''), RUSTC=str(RUST / 'rustc'), RUSTDOC=str(RUST / 'rustdoc'), CARGO_TARGET_DIR=str(target), FRD_PIPELINE_DATA=str(DATA / 'workloads.bin'), FRD_PIPELINE_WARM_REF=str(DATA / 'warm-reference.bin'))
    if observe:
        env['RUSTFLAGS'] = '--cfg frd_pipeline_alloc --check-cfg=cfg(frd_pipeline_alloc)'
    return env

def run(cmd, log, env):
    OUT.mkdir(parents=True, exist_ok=True)
    print('RUN', log.name, ' '.join(str(c) for c in cmd), flush=True)
    with log.open('w') as out:
        result = subprocess.run([str(c) for c in cmd], cwd=ROOT, env=env, stdout=out, stderr=subprocess.STDOUT)
    if result.returncode:
        print(log.read_text()[-12000:])
        raise RuntimeError(f'exit={result.returncode}; {log}')
    return log.read_text()

def cargo_build(original=False, observe=False):
    target = ROOT / ('target/pipeline-oracle-rust196' if original else 'target/pipeline-observed-rust196' if observe else 'target/pipeline-rust196')
    env = environment(target, observe)
    cmd = [RUST / 'cargo', 'build' if original else 'test', '--offline', '--locked', '--release', '--message-format=json']
    if original:
        cmd += ['--manifest-path', ROOT / 'tests/legacy-oracle/Cargo.toml', '--bin', 'pipeline']
    else:
        cmd += ['-p', 'frd-protocol-rdp', '--lib', '--no-run']
    text = run(cmd, OUT / ('build-original.log' if original else 'build-observed.log' if observe else 'build-current.log'), env)
    executables = []
    for line in text.splitlines():
        if not line.startswith('{'):
            continue
        record = json.loads(line)
        if record.get('reason') == 'compiler-artifact' and record.get('executable'):
            executables.append(record['executable'])
    assert len(executables) == 1, executables
    return Path(executables[0]), env

def isolation():
    env = environment(ROOT / 'target/pipeline-oracle-rust196')
    graph = json.loads(subprocess.check_output([RUST / 'cargo', 'metadata', '--offline', '--locked', '--format-version=1', '--manifest-path', ROOT / 'tests/legacy-oracle/Cargo.toml'], cwd=ROOT, env=env))
    for package in graph['packages']:
        if package['name'] == 'frd-legacy-original-oracle':
            continue
        assert package['source'] and package['source'].startswith('registry+'), package['name']
        assert not package['name'].startswith('frd-')
    inventory_path = ROOT / 'tests/legacy-oracle/SOURCE-INVENTORY.json'
    inventory = json.loads(inventory_path.read_text())
    packages = {p['name'] + '-' + p['version']: p for p in graph['packages']}
    for name, entry in inventory.items():
        base = Path(packages[name]['manifest_path']).parent
        archive = base.parents[2] / 'cache' / base.parent.name / (name + '.crate')
        assert digest(archive) == entry['archive_sha256'], archive
        for relative, expected in entry['files'].items():
            assert digest(base / relative) == expected, base / relative
    session = next(p for p in graph['packages'] if p['name'] == 'ironrdp-session')
    features = next(n['features'] for n in graph['resolve']['nodes'] if n['id'] == session['id'])
    assert 'qoi' not in features and 'qoiz' not in features
    return {'source_inventory_sha256': digest(inventory_path), 'graph': 'registry only; all pinned source/archive hashes verified; no production path imports', 'session_features': features, 'bulk': 'Stage compression_type=None / packet compression_flags=None: measured no-bulk only'}

def verify_data():
    manifest = json.loads((DATA / 'manifest.json').read_text())
    assert digest(DATA / 'workloads.bin') == manifest['bundle_sha256']
    source = ROOT / 'tests/legacy-oracle/src/bin/pipeline.rs'
    if 'annotation' in manifest:
        annotation = manifest['annotation']
        base = Path(annotation['base_directory'])
        assert digest(base / 'workloads.bin') == annotation['base_bundle_sha256']
        assert digest(base / 'manifest.json') == annotation['base_manifest_sha256']
        assert digest(base / 'original-producer.rs') == manifest['producer_source_sha256'] == annotation['original_producer_source_sha256']
        assert digest(base / 'original-producer') == annotation['original_producer_executable_sha256']
        assert digest(source) == annotation['annotation_source_sha256']
        assert digest(ROOT / 'target/pipeline-oracle-rust196/release/pipeline') == annotation['annotation_executable_sha256']
        assert hashlib.sha256(annotation['recipe'].encode()).hexdigest() == annotation['recipe_sha256']
        assert annotation['equivalence']['all_non_geometry_bytes_identical']
        assert annotation['equivalence']['all_original_expect_blobs_identical']
    else:
        assert digest(source) == manifest['producer_source_sha256']
    if 'warm_reference' in manifest:
        warm = manifest['warm_reference']
        assert digest(DATA / warm['file']) == warm['sha256']
        assert warm['base_bundle_sha256'] == manifest['bundle_sha256']
        assert digest(DATA / 'annotation-manifest-before-warm.json') == warm['base_annotation_manifest_sha256']
        assert digest(source) == warm['producer_source_sha256']
        assert digest(ROOT / 'target/pipeline-oracle-rust196/release/pipeline') == warm['producer_executable_sha256']
        assert len(warm['proof']) == 3
        assert all(p['all_steps_complete_rgba_and_outputs_equal'] and p['additional_verified_continuous_batches'] == 4096 for p in warm['proof'])
    return manifest

def invoke(elf, env, test, log):
    text = run([elf, PREFIX + test, '--exact', '--ignored', '--nocapture', '--test-threads=1'], OUT / log, env)
    assert re.findall(r'^running (\d+) test(?:s)?$', text, re.M) == ['1'], 'exact target must execute one test'
    assert re.findall(r'^test (\S+) \.\.\.', text, re.M) == [PREFIX + test], 'wrong exact target'
    assert len(re.findall(r'^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured;', text, re.M)) == 1, 'exact target must pass'
    return text

def proof_inputs():
    baseline = OUT / 'positive-baseline'
    assert digest(baseline / 'legacy_pipeline.rs') == 'b24f3b1b8ce2a9e840626e78f812c49ee49d6dfe6e046a13c711539ab8a67ede'
    assert digest(baseline / 'full-pass-elf') == '4a06211fbbc11b5a514a08dcebdaac4c19042c5d4beff6a0f44ec902b7dd37d6'
    log = OUT / 'correctness-v3-full.log'
    assert digest(log) == '388588012e86e13fd19983370d68772d501f4ea57a911b4fb58432d292391cf3'
    assert 'subset=false workloads=74 backends=2 frozen_sha=198c9e0a9ce86ff9c0ca8b916d36bb5eef9ad211dd6ffd3b306e3e396d19ae4b' in log.read_text()
    assert '1 passed; 0 failed' in log.read_text()
    source = ROOT / 'crates/frd-protocol-rdp/src/legacy_pipeline.rs'
    prefix = source.read_bytes().split(b'fn failure_lane(')[0]
    assert prefix == (baseline / 'legacy_pipeline.rs').read_bytes().split(b'fn failure_lane(')[0], 'real pipeline/correctness chain changed: full gate required'
    production = json.loads((baseline / 'production-source-hashes.json').read_text())
    for relative, expected in production.items():
        assert digest(ROOT / relative) == expected, ('production chain changed: full gate required', relative)
    assert digest(DATA / 'workloads.bin') == '198c9e0a9ce86ff9c0ca8b916d36bb5eef9ad211dd6ffd3b306e3e396d19ae4b'
    return {'complete_pass_log_sha256': digest(log), 'complete_pass_actual_elf_sha256': digest(baseline / 'full-pass-elf'), 'complete_pass_source_sha256': digest(baseline / 'legacy_pipeline.rs'), 'unchanged_real_pipeline_and_positive_check_prefix_sha256': hashlib.sha256(prefix).hexdigest(), 'production_baseline_inventory_sha256': digest(baseline / 'production-source-hashes.json'), 'production_source_hashes': production, 'bundle_sha256': digest(DATA / 'workloads.bin')}

def focused_truth_scopes(source):
    text = source.read_text()
    def function(name):
        body = text[text.index('fn ' + name + '('):]
        return body.split('\n#[test]', 1)[0].strip()
    parts = {'failure_and_warm_loader': text[text.index('fn failure_lane('):text.index('fn percentile(')], 'direct_stage_call_impl':text[text.index('impl StageLane {'):].split('\n#[test]',1)[0], 'regression_entry':function('pipeline_regression_discriminators'), 'steady_pipeline_entry':function('pipeline_steady_pointer_reference_gate')}
    return {key:hashlib.sha256(value.strip().encode()).hexdigest() for key,value in parts.items()}

def record_pass_receipt(elf, env, path, reuse=False):
    proof = proof_inputs()
    if reuse:
        baseline = OUT / 'post-steady-baseline'
        old = json.loads((baseline / 'receipt.json').read_text())
        assert digest(baseline / 'legacy_pipeline.rs') == old['final_test_source_sha256']
        for key,value in proof.items():
            assert old[key] == value
        for key,log in [('focused_log_sha256','correctness-final-focused.log'),('discriminator_log_sha256','correctness-final-discriminators.log'),('steady_log_sha256','correctness-final-steady.log')]:
            assert old[key] == digest(OUT / log)
        assert old['warm_reference_sha256'] == digest(DATA / 'warm-reference.bin')
        assert focused_truth_scopes(baseline / 'legacy_pipeline.rs') == focused_truth_scopes(ROOT / 'crates/frd-protocol-rdp/src/legacy_pipeline.rs'), 'actual loader/Stage/normal/steady proof code changed'
        proof['scoped_reuse_receipt_sha256'] = digest(baseline / 'receipt.json')
    else:
        invoke(elf, env, 'pipeline_original_checkpoints_quick_subset', 'correctness-final-focused.log')
        invoke(elf, env, 'pipeline_regression_discriminators', 'correctness-final-discriminators.log')
        invoke(elf, env, 'pipeline_steady_pointer_reference_gate', 'correctness-final-steady.log')
    invoke(elf, env, 'pipeline_stage_steady_pointer_reference_gate', 'correctness-final-stage-steady.log')
    proof.update(final_test_elf_sha256=digest(elf), final_test_source_sha256=digest(ROOT / 'crates/frd-protocol-rdp/src/legacy_pipeline.rs'), final_runner_sha256=digest(Path(__file__)), focused_log_sha256=digest(OUT / 'correctness-final-focused.log'), discriminator_log_sha256=digest(OUT / 'correctness-final-discriminators.log'), steady_log_sha256=digest(OUT / 'correctness-final-steady.log'), warm_reference_sha256=digest(DATA / 'warm-reference.bin'), scope='full74 old exact PASS + unchanged real-chain/positive-check prefix + production source inventory + strengthened failure/assertion focused PASS + independent stock4096 steady reference and actual C/N64+256 per-step outputs/RGBA/BGRX proof; observation/timing/loader functions changed outside prefix')
    path.write_text(json.dumps(proof, indent=2) + '\n')
    return proof

def verify_pass_receipt(elf, path):
    receipt = json.loads(path.read_text())
    for key, value in proof_inputs().items():
        assert receipt[key] == value, key
    assert receipt['final_test_elf_sha256'] == digest(elf)
    assert receipt['final_test_source_sha256'] == digest(ROOT / 'crates/frd-protocol-rdp/src/legacy_pipeline.rs')
    assert receipt['final_runner_sha256'] == digest(Path(__file__))
    assert receipt['focused_log_sha256'] == digest(OUT / 'correctness-final-focused.log')
    assert receipt['discriminator_log_sha256'] == digest(OUT / 'correctness-final-discriminators.log')
    assert receipt['steady_log_sha256'] == digest(OUT / 'correctness-final-steady.log')
    assert receipt['warm_reference_sha256'] == digest(DATA / 'warm-reference.bin')
    return digest(path)

def mutants():
    source = ROOT / 'crates/frd-protocol-rdp/src/legacy_pipeline.rs'
    parent = ROOT / 'crates/frd-protocol-rdp/src/active_session.rs'
    cases = [
        ('bypass-native-stage-binding', parent, 'resolved_session_selection(if native {', 'resolved_session_selection(if false {', 'pipeline_regression_discriminators', 'native 失败前不得消费 pending pointer'),
        ('spurious-startup', source, 'self.h.compile()', 'self.h.compile().into_iter().map(|t| match t { frd_frame::FrameTransaction::Revision {earliest_constituent_enqueue_at, revision} => frd_frame::FrameTransaction::Startup {earliest_constituent_enqueue_at, reset:frd_frame::FrameReset {session_id:self.h.session_id,generation:revision.generation,size:PixelSize {width:u32::from(w.width),height:u32::from(w.height)},format:PixelFormat::Bgrx8UnormSrgb},revision},t=>t}).collect()', 'pipeline_regression_discriminators', 'unique Startup'),
        ('skip-nonempty-update', source, '&s.data,\n            )', '&if self.started { fast_packet(&[]) } else {s.data.clone()},\n            )', 'pipeline_regression_discriminators', 'persistent compiler canvas'),
    ]
    results = []
    for name, path, old, new, test, signal in cases:
        before = path.read_bytes()
        text = before.decode()
        assert old in text, name
        try:
            path.write_text(text.replace(old, new, 1))
            elf, env = cargo_build()
            mutant_elf_sha256 = digest(elf)
            mutant_source_sha256 = digest(path)
            (OUT / f'mutant-{name}-build.log').write_bytes((OUT / 'build-current.log').read_bytes())
            exact = PREFIX + test
            cmd = [elf, exact, '--exact', '--nocapture', '--test-threads=1', '--ignored']
            log = OUT / f'mutant-{name}.log'
            with log.open('w') as out:
                r = subprocess.run([str(c) for c in cmd], cwd=ROOT, env=env, stdout=out, stderr=subprocess.STDOUT)
            result = log.read_text()
            assert r.returncode == 101 and 'test result: FAILED' in result and signal in result, (name, result[-5000:])
            results.append({'mutant': name, 'compiled': True, 'mutant_source_sha256': mutant_source_sha256, 'actual_mutant_elf_sha256': mutant_elf_sha256, 'restored_source_sha256': hashlib.sha256(before).hexdigest(), 'semantic_exit': r.returncode, 'required_assertion': signal, 'log': str(log), 'log_sha256': digest(log)})
            (OUT / 'mutants-progress.json').write_text(json.dumps(results, indent=2) + '\n')
        finally:
            path.write_bytes(before)
            assert path.read_bytes() == before
    (OUT / 'mutants.json').write_text(json.dumps(results, indent=2) + '\n')
    return results

def coordinated_stage(elf, env):
    manifest = verify_data()
    original = ROOT / 'target/pipeline-oracle-rust196/release/pipeline'
    original_env = environment(ROOT / 'target/pipeline-oracle-rust196')
    stage_rows, original_rows, calibration = {}, {}, []
    request = None
    for round_index in range(16):
        stage_env = env.copy()
        if request is not None:
            stage_env['FRD_PIPELINE_STAGE_REQUEST'] = str(request)
        stage_log = OUT / f'stage-round-{round_index}.log'
        text = invoke(elf, stage_env, 'pipeline_release_stage_measurements', stage_log.name)
        groups = {}
        for line in text.splitlines():
            marker = line.find('PIPELINE_STAGE')
            if marker >= 0:
                line = line[marker:]
                f = line.split('\t')
                groups.setdefault((f[1], int(f[2])), []).append(line)
        assert groups
        original_json = OUT / f'original-round-{round_index}.json'
        run([original, '--bench', DATA / 'workloads.bin', stage_log, original_json, DATA / 'warm-reference.bin'], OUT / f'original-round-{round_index}.log', original_env)
        measured = json.loads(original_json.read_text())
        requests = []
        for row in measured:
            key = (row['workload'], int(row['window']))
            lines = groups.pop(key)
            cn = [line.split('\t') for line in lines if line.startswith('PIPELINE_STAGE\t')]
            assert len(cn) == 2 and {f[3] for f in cn} == {'0', '1'}
            assert all(int(f[4]) == row['count'] and f[10] == row['checksum'] for f in cn)
            stage_rows[key] = lines
            row['granularity_100ms'] = row['total_ns'] >= 100_000_000
            row['cap_reached'] = row['count'] == 4096
            row['actual_original_executable_sha256'] = digest(original)
            original_rows[key] = row
            if not row['granularity_100ms'] and not row['cap_reached']:
                requests.append((key[0], key[1], row['count'] + 256))
        assert not groups
        calibration.append({'round': round_index, 'groups': len(measured), 'uniform_retry_groups': requests, 'cn_log_sha256': digest(stage_log), 'original_json_sha256': digest(original_json)})
        print(f'STAGE_COORDINATION round={round_index} groups={len(measured)} below_100ms_retry={len(requests)}', flush=True)
        if not requests:
            break
        request = OUT / f'stage-request-{round_index + 1}.tsv'
        request.write_text(''.join(f'{name}\t{window}\t{count}\n' for name, window, count in requests))
    else:
        raise RuntimeError('bounded uniform Stage coordination exhausted')
    merged = '\n'.join(line for key in sorted(stage_rows) for line in stage_rows[key]) + '\n'
    (OUT / 'measure-stage.log').write_text(merged)
    original_json = OUT / 'measure-original.json'
    original_json.write_text(json.dumps([original_rows[k] for k in sorted(original_rows)], indent=2) + '\n')
    (OUT / 'stage-calibration.json').write_text(json.dumps(calibration, indent=2) + '\n')
    return merged, original_json

def verify_runner_delta(elf):
    base = OUT / 'initial-unbalanced-baseline'
    old_path = base / 'sources/tools/tests/run_legacy_pipeline.py'
    old_receipt_path = base / 'exact-pass-receipt.json'
    old_receipt = json.loads(old_receipt_path.read_text())
    assert digest(old_path) == old_receipt['final_runner_sha256']
    snapshot = json.loads((base / 'snapshot.json').read_text())
    for relative, expected in snapshot.items():
        if relative.endswith('-executable-original-path'):
            continue
        assert digest(base / relative) == expected, relative
    for relative in ('crates/frd-protocol-rdp/src/active_session.rs', 'crates/frd-protocol-rdp/src/legacy_pipeline.rs', 'tests/legacy-oracle/src/bin/pipeline.rs', 'tests/legacy-oracle/Cargo.toml', 'tests/legacy-oracle/fixtures/pipeline-summary.json'):
        assert digest(ROOT / relative) == digest(base / 'sources' / relative), relative
    assert digest(elf) == digest(base / 'normal-elf') == old_receipt['final_test_elf_sha256']
    def proof_ast(path):
        names = {'proof_inputs', 'focused_truth_scopes', 'record_pass_receipt', 'verify_pass_receipt'}
        return {n.name: ast.dump(n, include_attributes=False) for n in ast.parse(path.read_text()).body if isinstance(n, ast.FunctionDef) and n.name in names}
    assert len(proof_ast(old_path)) == 4 and proof_ast(old_path) == proof_ast(Path(__file__))
    spec = importlib.util.spec_from_file_location('frozen_initial_pipeline_runner', old_path)
    old = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(old)
    old.ROOT, old.OUT, old.DATA, old.RUST = ROOT, OUT, DATA, RUST
    assert Path(old.__file__).resolve() == old_path.resolve()
    old_verified = old.verify_pass_receipt(elf, old_receipt_path)
    assert old_verified == digest(old_receipt_path)
    for key, value in proof_inputs().items():
        assert old_receipt[key] == value
    saved = {key:os.environ.get(key) for key in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')}
    try:
        os.environ['RUSTFLAGS'] = '--cfg frd_pipeline_alloc'
        os.environ['CARGO_ENCODED_RUSTFLAGS'] = '--cfg\x1ffrd_conflicting_ambient_marker'
        normal, normal_env = cargo_build()
        observed, observed_env = cargo_build(observe=True)
        def fingerprint(binary):
            path = binary.parents[1] / '.fingerprint' / ('frd-protocol-rdp-' + binary.name.rsplit('-', 1)[-1]) / 'test-lib-frd_protocol_rdp.json'
            return path, json.loads(path.read_text())
        normal_fp, normal_config = fingerprint(normal)
        observed_fp, observed_config = fingerprint(observed)
        assert normal_config['rustflags'] == []
        assert observed_config['rustflags'] == ['--cfg', 'frd_pipeline_alloc', '--check-cfg=cfg(frd_pipeline_alloc)']
        assert digest(normal) == digest(base / 'normal-elf')
        assert digest(observed) == digest(base / 'observed-elf')
        normal_list = run([normal, '--list'], OUT / 'delta-normal-test-list.log', normal_env)
        observed_list = run([observed, '--list'], OUT / 'delta-observed-test-list.log', observed_env)
        assert PREFIX + 'pipeline_observed_allocations: test' not in normal_list
        assert observed_list.count(PREFIX + 'pipeline_observed_allocations: test') == 1
        rejected = False
        try:
            invoke(normal, normal_env, 'nonexistent_exact_fail_closed_probe', 'delta-zero-tests.log')
        except AssertionError as error:
            rejected = str(error) == 'exact target must execute one test'
        zero_log = (OUT / 'delta-zero-tests.log').read_text()
        assert rejected and 'running 0 tests' in zero_log and '0 passed; 0 failed' in zero_log
        invoke(normal, normal_env, 'pipeline_regression_discriminators', 'delta-positive-exact-one.log')
        proof = {'old_module_file':str(old.__file__), 'old_module_sha256':digest(old_path), 'explicit_relocated_globals':{'ROOT':str(ROOT),'OUT':str(OUT),'DATA':str(DATA),'RUST':str(RUST)}, 'old_receipt_sha256':old_verified, 'unchanged_four_proof_function_asts':True, 'actual_zero_test_exit':0, 'zero_tests_rejected':rejected, 'conflicting_ambient_flags':{'RUSTFLAGS':'--cfg frd_pipeline_alloc','CARGO_ENCODED_RUSTFLAGS':'--cfg U+001F frd_conflicting_ambient_marker'}, 'normal_effective_rustflags':normal_config['rustflags'], 'observed_effective_rustflags':observed_config['rustflags'], 'normal_fingerprint_sha256':digest(normal_fp),'observed_fingerprint_sha256':digest(observed_fp),'actual_normal_elf_sha256':digest(normal),'actual_observed_elf_sha256':digest(observed), 'logs':{name:digest(OUT / name) for name in ('delta-normal-test-list.log','delta-observed-test-list.log','delta-zero-tests.log','delta-positive-exact-one.log')}}
        (OUT / 'runner-fail-closed-delta.json').write_text(json.dumps(proof, indent=2) + '\n')
        updated = dict(old_receipt)
        updated.update(final_runner_sha256=digest(Path(__file__)), runner_delta_proof_sha256=digest(OUT / 'runner-fail-closed-delta.json'), immutable_old_receipt_sha256=old_verified)
        path = OUT / 'exact-pass-receipt.json'
        path.write_text(json.dumps(updated, indent=2) + '\n')
        assert verify_pass_receipt(normal, path) == digest(path)
        assert digest(old_receipt_path) == old_verified
        print('RUNNER_DELTA PASS old module actually verified; zero-test rejected; exact-one positive; normal/observed conflicting flags isolated; new receipt verified', flush=True)
        return proof
    finally:
        for key,value in saved.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value

def supplement_stage(elf, env):
    base = OUT / 'initial-unbalanced-baseline'
    verify_pass_receipt(elf, OUT / 'exact-pass-receipt.json')
    initial = (base / 'measure-stage.log').read_text()
    groups = {}
    for line in initial.splitlines():
        fields = line.split('\t')
        groups.setdefault((fields[1], int(fields[2])), []).append(line)
    # 初轮 harness 把首个 cold 标记粘到 test 名称后；从原始日志补回该真实行。
    recovered_cold = []
    for line in (base / 'stage-round-0.log').read_text().splitlines():
        marker = line.find('PIPELINE_STAGE_COLD\t')
        if marker >= 0:
            line = line[marker:]; fields = line.split('\t'); key = (fields[1], int(fields[2]))
            if line not in groups[key] and not any(x.startswith('PIPELINE_STAGE_COLD\t') and x.split('\t')[3] == fields[3] for x in groups[key]):
                groups[key].insert(0, line); recovered_cold.append(line)
    selected = {key:lines for key,lines in groups.items() if key[1] == 1}
    assert len(selected) == 58
    requests = {key:int(next(line.split('\t')[4] for line in lines if line.startswith('PIPELINE_STAGE\t'))) for key,lines in selected.items()}
    original_rows = {(r['workload'],int(r['window'])):r for r in json.loads((base / 'measure-original.json').read_text())}
    original = ROOT / 'target/pipeline-oracle-rust196/release/pipeline'
    calibration = []
    for round_index in range(16):
        request_log = OUT / f'balanced-window1-request-{round_index}.log'
        request_lines = []
        for key,count in sorted(requests.items()):
            assert 256 <= count <= 4096 and count % 256 == 0
            for line in selected[key]:
                if line.startswith('PIPELINE_STAGE\t'):
                    fields=line.split('\t'); fields[4]=str(count); request_lines.append('\t'.join(fields))
        request_log.write_text('\n'.join(request_lines)+'\n')
        original_json = OUT / f'balanced-window1-original-{round_index}.json'
        original_log = OUT / f'balanced-window1-original-{round_index}.log'
        run([original,'--bench',DATA/'workloads.bin',request_log,original_json,DATA/'warm-reference.bin'], original_log, environment(ROOT/'target/pipeline-oracle-rust196'))
        request_tsv = OUT / f'balanced-window1-request-{round_index}.tsv'
        request_tsv.write_text(''.join(f'{name}\t{window}\t{count}\n' for (name,window),count in sorted(requests.items())))
        cn_env=env.copy(); cn_env['FRD_PIPELINE_STAGE_REQUEST']=str(request_tsv)
        cn_log=OUT / f'balanced-window1-cn-{round_index}.log'
        text=invoke(elf,cn_env,'pipeline_release_stage_measurements',cn_log.name)
        measured_groups={}
        for line in text.splitlines():
            marker=line.find('PIPELINE_STAGE')
            if marker >= 0:
                line=line[marker:]; fields=line.split('\t')
                measured_groups.setdefault((fields[1],int(fields[2])),[]).append(line)
        measured=json.loads(original_json.read_text()); assert len(measured)==len(requests)
        retry={}
        for row in measured:
            key=(row['workload'],int(row['window'])); assert key in requests
            lines=measured_groups.pop(key)
            cn=[line.split('\t') for line in lines if line.startswith('PIPELINE_STAGE\t')]
            assert len(cn)==2 and {f[3] for f in cn}=={'0','1'}
            cn_count=int(cn[0][4]); assert cn_count==int(cn[1][4]) and cn_count>=requests[key]
            assert all(f[10]==row['checksum'] for f in cn)
            equal=row['count']==cn_count
            enough=all(int(f[9])>=100_000_000 for f in cn) and row['total_ns']>=100_000_000
            if not equal or (not enough and cn_count<4096):
                retry[key]=min(4096,max(cn_count,row['count'])+256)
            else:
                assert equal and (enough or cn_count==4096)
                selected[key]=lines
                row.update(granularity_100ms=row['total_ns']>=100_000_000,cap_reached=row['count']==4096,actual_original_executable_sha256=digest(original),execution_order='O before C/N; window1 N-first')
                original_rows[key]=row
        assert not measured_groups
        calibration.append({'round':round_index,'groups':len(measured),'actual_phase_order':['O','N/C'],'request_log_sha256':digest(request_log),'request_tsv_sha256':digest(request_tsv),'original_log_sha256':digest(original_log),'original_json_sha256':digest(original_json),'cn_log_sha256':digest(cn_log),'uniform_retry_groups':[[name,window,count] for (name,window),count in sorted(retry.items())]})
        print(f'STAGE_BALANCED window1 round={round_index} groups={len(measured)} retry={len(retry)} O-before-N/C',flush=True)
        if not retry:
            break
        requests=retry
    else:
        raise RuntimeError('bounded order supplement exhausted')
    groups.update(selected)
    merged='\n'.join(line for key in sorted(groups) for line in groups[key])+'\n'
    (OUT/'measure-stage.log').write_text(merged)
    original_json=OUT/'measure-original.json'
    original_json.write_text(json.dumps([original_rows[key] for key in sorted(original_rows)],indent=2)+'\n')
    order={'scope':'Stage-only window1 supplement; full/control/observed unchanged','windows':{'0':'initial C/N before O; C-first','1':'supplement O before C/N; N-first','2':'initial C/N before O; C-first'},'initial_snapshot_sha256':digest(base/'snapshot.json'),'initial_stage_sha256':digest(base/'measure-stage.log'),'initial_original_sha256':digest(base/'measure-original.json'),'initial_harness_prefix_cold_rows_recovered_from_raw_log':recovered_cold,'rounds':calibration}
    (OUT/'stage-order-supplement.json').write_text(json.dumps(order,indent=2)+'\n')
    summary(original_json,(base/'measure-full.log').read_text(),merged,(base/'measure-control.log').read_text(),order)
    return order

def summary(original_json, full_log, stage_log, control_log, stage_order=None):
    rows = []
    for log in [full_log, stage_log, control_log]:
        for line in log.splitlines():
            marker = line.find('PIPELINE_')
            if marker >= 0:
                rows.append(line[marker:].split('\t'))
    input_bytes = {(r[1],r[2],r[3]):int(r[4]) for r in rows if r[0]=='PIPELINE_INPUT_BYTES'}
    original = json.loads(original_json.read_text())
    budget_misses = [r for r in rows if (r[0] == 'PIPELINE_RATIO' and r[-1] == 'false') or (r[0] == 'PIPELINE_WARM' and r[-2] == 'false') or (r[0] == 'PIPELINE_COLD' and r[-1] == 'false') or (r[0] == 'PIPELINE_CONTROL' and len(r) == 7 and r[-1] == 'false')]
    derived = []
    for r in rows:
        if r[0] == 'PIPELINE_WARM':
            total = int(r[9])
            derived.append({'workload': r[1], 'window': int(r[2]), 'policy': r[3], 'decoded_pixels_per_second': int(r[10]) * 1e9 / total, 'committed_pixels_per_second': int(r[11]) * 1e9 / (4 * total), 'input_bytes_per_second': input_bytes[(r[1],r[2],r[3])] * 1e9 / total, 'queue_metric': 'sum of mailbox occupancy sampled before drain outside timed calls; not push outcomes or enqueue-event count'})
    stage_costs = {(r[1],int(r[2]),int(r[3])): r for r in rows if r[0] == 'PIPELINE_STAGE_COST'}
    stage = []
    ratios = []
    o = {(r['workload'],int(r['window'])):r for r in original}
    stage_cn = {}
    for r in rows:
        if r[0] == 'PIPELINE_STAGE':
            key=(r[1],int(r[2]),int(r[3]));c=stage_costs[key];total=int(r[9])
            stage_cn[key]=r
            stage.append({'workload':r[1],'window':int(r[2]),'policy':int(r[3]),'count':int(r[4]),'actual_calls':int(c[7]),'source_pixels':int(c[4]),'input_bytes':int(c[5]),'wire_declared_geometry_apply_pixels':int(c[6]),'granularity_100ms':c[8]=='true','cap_reached':c[9]=='true','decoded_pixels_per_second':int(c[4])*1e9/total,'input_bytes_per_second':int(c[5])*1e9/total})
    for key, row in o.items():
        c=stage_cn[(*key,0)];n=stage_cn[(*key,1)]
        cost=stage_costs[(*key,0)]
        assert row['count']==int(c[4])==int(n[4])
        row.update(source_pixels=int(cost[4]),input_bytes=int(cost[5]),actual_calls=int(cost[7]),wire_declared_geometry_apply_pixels=int(cost[6]),cost_basis='same frozen Action/PDU workload and exact uniform batch count; no Surface publication in O Stage')
        ratios.append({'workload':key[0],'window':key[1],'native_to_compat_p95':int(n[6])/int(c[6]),'native_to_original_p95':int(n[6])/row['p95_ns'],'compat_to_original_p95':int(c[6])/row['p95_ns'],'scope':'Stage-only O/C/N identical packets/counts/image checksum; never O Stage to full pipeline speedup'})
    notes = {'derived_throughput': derived, 'stage_costs_and_throughput':stage, 'stage_ratios':ratios,'correctness': 'exact full-pass receipt and strengthened final focused proof passed before timing; each measured batch rechecks independent checksum outside clocks', 'qualification': 'diagnostic local processing only; never whole-production qualification', 'windows': 3, 'minimum_warmup': 64, 'minimum_measured': 256, 'maximum_measured': 4096, 'count_unit':'whole encoded warm batch, plus actual Action/PDU calls separately; C/N and O uniform per workload/window', 'original_stage': original, 'current_rows': rows, 'budget_misses': budget_misses, 'cold_budget':'setup plus first successful Startup total <=250000000ns; real misses retained','omitted_runtime':'initial no-bulk, no QOI/QOIZ, no GPU/network/screen/live sessions; host ARM64 only'}
    notes['stage_budget_misses'] = [r for r in ratios if r['native_to_compat_p95'] > 1.25]
    notes['stage_execution_order'] = stage_order or {'scope':'initial diagnostic: C/N three windows completed before O three windows'}
    initial_path = OUT / 'initial-unbalanced-baseline/measurements.json'
    if stage_order and initial_path.exists():
        initial = json.loads(initial_path.read_text())
        notes['initial_stage_budget_misses'] = [r for r in initial['stage_ratios'] if r['native_to_compat_p95'] > 1.25]
        notes['immutable_initial_measurements_sha256'] = digest(initial_path)
    (OUT / 'measurements.json').write_text(json.dumps(notes, indent=2) + '\n')

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--generate', action='store_true')
    p.add_argument('--generate-small-rfx', action='store_true', help='fresh original Stage producer for six bounded RFX workloads')
    p.add_argument('--annotate-from', type=Path)
    p.add_argument('--quick', action='store_true', help='explicit small/RFX/vendor/pointer/failure/control subset; never timing permission')
    p.add_argument('--check', action='store_true')
    p.add_argument('--measure', action='store_true')
    p.add_argument('--observe', action='store_true')
    p.add_argument('--mutants', action='store_true')
    p.add_argument('--record-pass', action='store_true', help='exact full-pass provenance plus unchanged real-chain and final focused proof')
    p.add_argument('--reuse-focused', action='store_true', help='reuse only exact unchanged verified loader/Stage/failure/steady scopes, sidecar and proof logs')
    p.add_argument('--warm-reference', action='store_true', help='independent stock4096 continuous warm batch proof and per-step sidecar')
    p.add_argument('--verify-runner-delta', action='store_true', help='immutable initial receipt/module plus actual zero-test/flags/exact-one proof; no timing')
    p.add_argument('--stage-order-supplement', action='store_true', help='only Stage window1 O-before-N/C; preserve other initial windows and full/control')
    p.add_argument('--pass-receipt', type=Path, default=OUT / 'exact-pass-receipt.json')
    p.add_argument('--data', type=Path, default=DATA, help='Explicit independent frozen data directory')
    args = p.parse_args()
    globals()['DATA'] = args.data.resolve()
    label = DATA.name.removeprefix('pipeline-workloads-')
    OUT.mkdir(parents=True, exist_ok=True)
    compiler = subprocess.check_output([RUST / 'rustc', '-vV'], text=True)
    assert compiler.startswith('rustc 1.96.0 ')
    info = {'compiler': compiler, 'original_isolation': isolation()}
    if args.annotate_from:
        elf, env = cargo_build(original=True)
        run([elf, '--annotate-from', args.annotate_from.resolve(), DATA], OUT / f'annotate-metadata-{label}.log', env)
    if args.generate or args.generate_small_rfx:
        elf, env = cargo_build(original=True)
        command = [elf, '--generate-small-rfx', DATA] if args.generate_small_rfx else [elf, DATA]
        run(command, OUT / f'generate-original-{label}.log', env)
        manifest = verify_data()
        (ROOT / 'tests/legacy-oracle/fixtures/pipeline-summary.json').write_text(json.dumps(manifest, indent=2) + '\n')
    if args.warm_reference:
        elf, env = cargo_build(original=True)
        run([elf, '--warm-reference', DATA, DATA / 'warm-reference.bin'], OUT / f'generate-warm-reference-{label}.log', env)
    manifest = verify_data()
    (ROOT / 'tests/legacy-oracle/fixtures/pipeline-summary.json').write_text(json.dumps(manifest, indent=2) + '\n')
    info.update(bundle_sha256=manifest['bundle_sha256'], owned_sources={str(f.relative_to(ROOT)): digest(f) for f in OWNED})
    if args.check or args.quick or args.measure or args.record_pass or args.mutants or args.verify_runner_delta or args.stage_order_supplement:
        elf, env = cargo_build()
        info.update(current_test_elf=str(elf), current_test_elf_sha256=digest(elf))
        if args.quick:
            invoke(elf, env, 'pipeline_original_checkpoints_quick_subset', f'correctness-{label}-quick.log')
        if args.check:
            invoke(elf, env, 'pipeline_original_checkpoints_real_factory_to_canvas', f'correctness-{label}-full.log')
    if args.mutants:
        info['mutants'] = mutants()
        elf, env = cargo_build()
        invoke(elf, env, 'pipeline_regression_discriminators', 'correctness-restored-discriminators.log')
        info.update(current_test_elf=str(elf), current_test_elf_sha256=digest(elf))
    if args.record_pass:
        info['exact_pass_receipt'] = record_pass_receipt(elf, env, args.pass_receipt, args.reuse_focused)
    if args.verify_runner_delta:
        info['runner_delta'] = verify_runner_delta(elf)
    if args.stage_order_supplement:
        info['stage_order_supplement'] = supplement_stage(elf, env)
    if args.measure:
        info['verified_pass_receipt_sha256'] = verify_pass_receipt(elf, args.pass_receipt)
        stage, original_json = coordinated_stage(elf, env)
        control = invoke(elf, env, 'pipeline_release_control_measurements', 'measure-control.log')
        full = invoke(elf, env, 'pipeline_release_measurements', 'measure-full.log')
        summary(original_json, full, stage, control)
    if args.observe:
        final, _ = cargo_build()
        info['verified_pass_receipt_sha256'] = verify_pass_receipt(final, args.pass_receipt)
        observed, observed_env = cargo_build(observe=True)
        invoke(observed, observed_env, 'pipeline_observed_allocations', 'observed-allocations.log')
    (OUT / 'identity.json').write_text(json.dumps(info, indent=2) + '\n')
    print('PASS: requested independent pipeline operations; identity:', OUT / 'identity.json')

if __name__ == '__main__':
    main()
