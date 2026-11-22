import argparse
import copy
import hashlib
import json
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace

import checkpoint
import checkpoint_phase
import checkpoint_meter
import run

ORIGINAL_LAUNCH = checkpoint_phase.launch
TIMEOUT_SECONDS = 20
POLL_SECONDS = 0.02


def wait_for(path):
    for _ in range(int(TIMEOUT_SECONDS / POLL_SECONDS)):
        if os.path.exists(path):
            return
        time.sleep(POLL_SECONDS)
    raise RuntimeError('drill barrier missing: ' + path)


def mock_sh(base, command, **kwargs):
    value = SimpleNamespace(returncode=0, stdout='', stderr='')
    if command[:3] == ['docker', 'image', 'inspect']:
        value.stdout = json.dumps([{'Id': 'sha256:mock-image', 'Config': {'WorkingDir': '/app'}}])
    elif command[:2] == ['docker', 'inspect']:
        marker = os.path.join(base, command[2] + '.container')
        if not os.path.exists(marker):
            value.returncode = 1
        else:
            value.stdout = json.dumps([checkpoint.read(marker)])
    elif command[:2] == ['docker', 'run']:
        name = command[command.index('--name') + 1]
        mounts = []
        for index, arg in enumerate(command):
            if arg == '-v':
                source, destination, *_ = command[index + 1].split(':')
                mounts.append({'Source': source, 'Destination': destination})
        checkpoint.atomic(os.path.join(base, name + '.container'), {'Image': 'sha256:mock-image', 'State': {'Running': True}, 'Mounts': mounts})
    elif command[:2] == ['docker', 'start']:
        marker = os.path.join(base, command[2] + '.container')
        value = checkpoint.read(marker)
        value['State']['Running'] = True
        checkpoint.atomic(marker, value)
        value = SimpleNamespace(returncode=0, stdout='', stderr='')
    elif command[:2] == ['docker', 'stop']:
        marker = os.path.join(base, command[-1] + '.container')
        info = checkpoint.read(marker)
        info['State']['Running'] = False
        checkpoint.atomic(marker, info)
    elif command[:2] == ['docker', 'cp'] and command[2].endswith(':/logs/verifier'):
        os.makedirs(command[3], exist_ok=True)
        with open(os.path.join(command[3], 'reward.txt'), 'w') as handle:
            handle.write('1')
    return value


def launch(base, sh, name, row, env, argv, stage, token):
    journal = os.path.join(base, 'campaign', 'rows', row['id'], 'journal')
    subprocess.Popen([sys.executable, os.path.abspath(__file__), '--phase-worker', journal, stage, token],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)


def phase_worker(journal, stage, token):
    directory = os.path.dirname(journal)
    with open(os.path.join(directory, stage + '.dispatches'), 'a') as handle:
        handle.write('dispatch\n')
        handle.flush()
        os.fsync(handle.fileno())
    if stage == 'agent':
        with open(os.path.join(directory, 'requests.jsonl'), 'a') as handle:
            handle.write(json.dumps({'path': '/responses', 'status': 200, 'usage': {'input_tokens': 3, 'output_tokens': 1}}) + '\n')
            handle.flush()
            os.fsync(handle.fileno())
    checkpoint.atomic(os.path.join(directory, stage + '.active'), {'pid': os.getpid()})
    wait_for(os.path.join(directory, stage + '.release'))
    checkpoint.atomic(os.path.join(journal, stage + '.receipt.json'), {'exit': 0, 'token': token})


def child(base, row_index):
    args = SimpleNamespace(**checkpoint.read(os.path.join(base, 'args.json')))
    run.sh = lambda command, **kwargs: mock_sh(base, command, **kwargs)
    checkpoint_phase.launch = lambda *arguments: launch(base, *arguments)
    rows = run.matrix(args, ['hl-agent'])
    identity = run.preflight(args, rows)
    campaign = checkpoint.Campaign(args.out, identity, True)
    args.campaign_identity = campaign.identity
    if row_index == 2 and not os.path.exists(os.path.join(base, 'partial.release')):
        original = checkpoint.atomic
        def interrupted(path, value, exclusive=False):
            if path.endswith('/result.json'):
                with open(path + '.partial', 'w') as handle:
                    handle.write('{"outcome":')
                    handle.flush()
                    os.fsync(handle.fileno())
                original(os.path.join(base, 'partial.active'), {'pid': os.getpid()})
                wait_for(os.path.join(base, 'partial.release'))
            return original(path, value, exclusive)
        checkpoint.atomic = interrupted
    result = run.run_row(rows[row_index], args, '127.0.0.1', {}, threading.Lock(), args.agent_root, os.path.join(args.out, 'rows'))
    campaign.close()
    if result['outcome'] != 'passed':
        raise RuntimeError(str(result))


def spawn(base, index):
    return subprocess.Popen([sys.executable, os.path.abspath(__file__), '--child', base, str(index)], stdout=subprocess.DEVNULL)


def release(path):
    checkpoint.atomic(path, True)


def resume_drill(base):
    task = os.path.join(base, 'tasks', 'mock', 'task')
    os.makedirs(os.path.join(task, 'tests'))
    with open(os.path.join(task, 'task.toml'), 'w') as handle:
        handle.write('[environment]\ndocker_image="mock:1"\n[agent]\ntimeout_sec=20\n[verifier]\ntimeout_sec=20\n')
    for name in ('instruction.md', 'tests/test.sh'):
        with open(os.path.join(task, name), 'w') as handle:
            handle.write('mock fixture\n')
    suite = os.path.join(base, 'suite.txt')
    with open(suite, 'w') as handle:
        handle.write('mock task\n')
    agent_root = os.path.join(base, 'agent-root')
    os.makedirs(os.path.join(agent_root, 'bin'))
    with open(os.path.join(agent_root, 'bin', 'hl-agent'), 'w') as handle:
        handle.write('mock binary\n')
    args = SimpleNamespace(suite=suite, tasks=os.path.join(base, 'tasks'), agent_root=agent_root,
        out=os.path.join(base, 'campaign'), model='mock-no-model', agents='hl-agent', attempts=3,
        parallel=1, limits='task', agent_memory_mb=0, max_turns=None, meter_port=1, relay_port=2,
        only=None, ca_bundle=None, hl_agent_arg=[], openai_route='openai', meter_host=None)
    checkpoint.atomic(os.path.join(base, 'args.json'), vars(args))
    run.sh = lambda command, **kwargs: mock_sh(base, command, **kwargs)
    rows = run.matrix(args, ['hl-agent'])
    identity = run.preflight(args, rows)
    campaign = checkpoint.Campaign(args.out, identity, False)
    run.prepare_matrix(args, rows, campaign.identity)
    campaign.close()
    for index, row in enumerate(rows):
        out = os.path.join(args.out, 'rows', row['id'])
        process = spawn(base, index)
        wait_for(os.path.join(out, 'agent.active'))
        if index == 0:
            process.kill()
            process.wait()
            release(os.path.join(out, 'agent.release'))
            process = spawn(base, index)
        else:
            release(os.path.join(out, 'agent.release'))
        wait_for(os.path.join(out, 'verifier.active'))
        if index == 1:
            process.kill()
            process.wait()
            release(os.path.join(out, 'verifier.release'))
            process = spawn(base, index)
        else:
            release(os.path.join(out, 'verifier.release'))
        if index == 2:
            wait_for(os.path.join(base, 'partial.active'))
            process.kill()
            process.wait()
            release(os.path.join(base, 'partial.release'))
            process = spawn(base, index)
        if process.wait(timeout=TIMEOUT_SECONDS):
            raise RuntimeError('resumed runner failed')
    snapshots = {row['id']: open(os.path.join(args.out, 'rows', row['id'], 'result.json'), 'rb').read() for row in rows}
    for index, row in enumerate(rows):
        if spawn(base, index).wait(timeout=TIMEOUT_SECONDS):
            raise RuntimeError('completed replay failed')
        directory = os.path.join(args.out, 'rows', row['id'])
        assert snapshots[row['id']] == open(os.path.join(directory, 'result.json'), 'rb').read()
        assert open(os.path.join(directory, 'agent.dispatches')).read().splitlines() == ['dispatch']
        assert open(os.path.join(directory, 'verifier.dispatches')).read().splitlines() == ['dispatch']
    return args, rows, identity


def mutation_drill(args, rows, identity):
    mutations = ('source_commit', 'source_files', 'parameters', 'suite_sha256', 'agent_tree', 'tasks', 'configs_sha256', 'images', 'matrix', 'ca_sha256', 'transport_sha256', 'runtime', 'task_trees', 'cache', 'meter_ledger_schema', 'reporting_rules')
    for path in ('examples/codex_agent.hl', 'scripts/fixtures/prompts/codex-instructions.md', 'scripts/fixtures/prompts/codex-texts.json'):
        assert path in identity['source_files'], 'missing canonical treatment pin: ' + path
    for key in mutations:
        changed = copy.deepcopy(identity)
        changed[key] = 'mutated'
        try:
            checkpoint.Campaign(args.out, changed, True)
        except checkpoint.ResumeError:
            pass
        else:
            raise AssertionError('identity mutation accepted: ' + key)
    changed = copy.copy(args)
    changed.attempts = 4
    try:
        run.preflight(changed, run.matrix(changed, ['hl-agent']))
    except checkpoint.ResumeError:
        pass
    else:
        raise AssertionError('matrix extension accepted')
    for path in ('examples/codex_agent.hl', 'scripts/fixtures/prompts/codex-instructions.md', 'scripts/fixtures/prompts/codex-texts.json'):
        changed = copy.deepcopy(identity)
        changed['source_files'][path] = 'mutated'
        try:
            checkpoint.Campaign(args.out, changed, True)
        except checkpoint.ResumeError:
            pass
        else:
            raise AssertionError('canonical source mutation accepted: ' + path)
    return len(mutations) + 4



def interruption_drill(base):
    directory = os.path.join(base, "interruption-only")
    state = checkpoint.Attempt(directory, {"campaign": "mock", "row": "interrupted"})
    journal = os.path.join(directory, "journal")
    checkpoint.durable_directory(journal)
    state.phase("agent", TIMEOUT_SECONDS)
    deadline = state.value["deadline"]
    calls = []
    def stopped(command, **kwargs):
        calls.append(command)
        return SimpleNamespace(returncode=0, stdout=json.dumps([{"State": {"Running": False}}]))
    try:
        checkpoint_phase.phase(stopped, "preserved", {"workdir": "/app"}, {}, ["must-not-run"], TIMEOUT_SECONDS, journal, "agent", state)
    except checkpoint.ResumeError as error:
        assert "execution not replayed" in str(error)
    else:
        raise AssertionError("stopped container silently resumed")
    assert all(command[:2] == ["docker", "inspect"] for command in calls)
    assert state.value["deadline"] == deadline
    state.value["deadline"] = time.time() - 1
    state.save()
    def running(command, **kwargs):
        calls.append(command)
        return SimpleNamespace(returncode=0, stdout=json.dumps([{"State": {"Running": True}}]))
    try:
        checkpoint_phase.phase(running, "preserved", {"workdir": "/app"}, {}, ["must-not-run"], TIMEOUT_SECONDS, journal, "agent", state)
    except checkpoint.ResumeError as error:
        assert "budget expired during interruption" in str(error)
    else:
        raise AssertionError("expired interrupted attempt was scored")
    receipt = os.path.join(journal, "agent.receipt.json")
    checkpoint.atomic(receipt, {"exit": 0, "token": checkpoint.digest([state.value["identity"], "agent"])})
    try:
        checkpoint_phase.phase(running, "preserved", {"workdir": "/app"}, {}, ["must-not-run"], TIMEOUT_SECONDS, journal, "agent", state)
    except checkpoint.ResumeError as error:
        assert "receipt after original deadline" in str(error)
    else:
        raise AssertionError("late completion was scored")
    assert all(command[:2] != ["docker", "exec"] for command in calls)
    return 3


def shell_receipt_drill(base):
    journal = os.path.join(base, "shell-journal")
    checkpoint.durable_directory(journal)
    processes = []
    def local_exec(command, **kwargs):
        index = command.index("sh", command.index("-w") + 2)
        shell = command[index:]
        shell[shell.index(checkpoint_phase.JOURNAL_MOUNT + "/agent")] = os.path.join(journal, "agent")
        processes.append(subprocess.Popen(shell, start_new_session=True))
    checkpoint_phase.launch = ORIGINAL_LAUNCH
    checkpoint_phase.launch(local_exec, "mock", {"workdir": "/"}, {}, ["sh", "-c", "printf receipt-ok; exit 7"], "agent", "shell-token")
    receipt = os.path.join(journal, "agent.receipt.json")
    wait_for(receipt)
    assert checkpoint.read(receipt) == {"exit": 7, "token": "shell-token"}
    assert open(os.path.join(journal, "agent.stdout")).read() == "receipt-ok"
    for process in processes:
        assert process.wait(timeout=TIMEOUT_SECONDS) == 0
    return 1



def stopped_drill(base, original_args):
    args = copy.copy(original_args)
    args.out = os.path.join(base, "stopped-campaign")
    args.attempts = 4
    run.sh = lambda command, **kwargs: mock_sh(base, command, **kwargs)
    rows = run.matrix(args, ["hl-agent"])
    identity = run.preflight(args, rows)
    campaign = checkpoint.Campaign(args.out, identity, False)
    args.campaign_identity = campaign.identity
    run.prepare_matrix(args, rows, campaign.identity)
    original_launch = checkpoint_phase.launch
    def immediate(sh, name, row, env, argv, stage, token):
        directory = os.path.join(args.out, "rows", row["id"])
        with open(os.path.join(directory, stage + ".dispatches"), "a") as handle:
            handle.write("dispatch\n")
        if stage == "agent":
            checkpoint.atomic(os.path.join(directory, "requests.json"), {"mock": True})
            with open(os.path.join(directory, "requests.jsonl"), "w") as handle:
                handle.write(json.dumps({"path": "/responses", "status": 200, "usage": {"input_tokens": 1, "output_tokens": 1}}) + "\n")
        checkpoint.atomic(os.path.join(directory, "journal", stage + ".receipt.json"), {"exit": 0, "token": token})
    checkpoint_phase.launch = immediate
    results = []
    retained = {}
    for index, row in enumerate(rows[:3]):
        directory = os.path.join(args.out, "rows", row["id"])
        state = checkpoint.Attempt(directory, {"campaign": campaign.identity, "row": row["id"]})
        state.value.update(image_id=args.image_ids[row["id"]], workdir="/app", accounting_status="observed")
        state.save()
        journal = os.path.join(directory, "journal")
        name = "nanoeval-" + hashlib.sha256(os.path.join(args.out, "rows").encode()).hexdigest()[:12] + "-" + row["id"]
        run.prepare_container(row, args, {}, args.agent_root, journal, name, state)
        state.phase("agent", TIMEOUT_SECONDS)
        if index == 1:
            state.value["agent_result"] = {"exit": 0, "timed_out": False, "wall_ms": 1}
            state.save()
            state.phase("verifier", TIMEOUT_SECONDS)
        if index == 2:
            immediate(None, name, row, {}, [], "agent", checkpoint.digest([state.value["identity"], "agent"]))
        marker = os.path.join(base, name + ".container")
        info = checkpoint.read(marker)
        info["State"]["Running"] = False
        checkpoint.atomic(marker, info)
        retained[row["id"]] = open(os.path.join(journal, "instruction.md"), "rb").read()
    for row in rows:
        result = run.run_row(row, args, "127.0.0.1", {}, threading.Lock(), args.agent_root, os.path.join(args.out, "rows"))
        results.append(result)
    assert [result.get("infrastructure_status") for result in results[:2]] == ["unresumable", "unresumable"]
    assert all(result['reporting_rules'] == run.reporting_rules.pin() for result in results)
    assert [result["outcome"] for result in results[2:]] == ["passed", "passed"]
    for index, row in enumerate(rows):
        directory = os.path.join(args.out, "rows", row["id"])
        if index < 2:
            assert not os.path.exists(os.path.join(directory, "result.json"))
            assert not os.path.exists(os.path.join(directory, "agent.dispatches"))
            assert not os.path.exists(os.path.join(directory, "verifier.dispatches"))
            assert results[index]["meter"]["unknown_requests"] is None
        if index < 3:
            assert retained[row["id"]] == open(os.path.join(directory, "journal", "instruction.md"), "rb").read()
        again = run.run_row(row, args, "127.0.0.1", {}, threading.Lock(), args.agent_root, os.path.join(args.out, "rows"))
        assert again == results[index]
    campaign.close()
    checkpoint_phase.launch = original_launch
    return {"stopped_containers": 3, "unresumable_unscored": 2, "postagent_exact_continuations": 1, "untouched_rows_completed": 1}

def service_command(base):
    return [sys.executable, os.path.abspath(__file__), "--service-worker", base]


def service_owner(base):
    with open(os.path.join(base, "service.log"), "a") as log:
        process = checkpoint_phase.service(service_command(base), log, dict(os.environ))
    checkpoint.atomic(os.path.join(base, "service.active"), {"pid": process.pid})
    wait_for(os.path.join(base, "owner.release"))


def service_drill(base):
    owner = subprocess.Popen([sys.executable, os.path.abspath(__file__), "--service-owner", base])
    wait_for(os.path.join(base, "service.active"))
    pid = checkpoint.read(os.path.join(base, "service.active"))["pid"]
    owner.kill()
    owner.wait()
    with open(os.path.join(base, "service.log"), "a") as log:
        recovered = checkpoint_phase.service(service_command(base), log, dict(os.environ))
    assert recovered.pid == pid and recovered.poll() is None
    release(os.path.join(base, "service.release"))
    return 1

def accounting_drill(base, sse=False):
    import http.client
    import meter
    results = []
    for index in range(3):
        run_id = ('terminal-sse-' if sse else 'accounting-') + str(index)
        directory = os.path.join(base, run_id)
        checkpoint.durable_directory(directory)
        entered, release_finish, consumed = threading.Event(), threading.Event(), threading.Event()
        original_finish, original_upstream = checkpoint_meter.finish, meter.upstream
        def paused_finish(*arguments):
            entered.set()
            assert release_finish.wait(TIMEOUT_SECONDS)
            original_finish(*arguments)
        class Response:
            status = 200
            sent = False
            def getheader(self, key):
                return 'text/event-stream' if sse else 'application/json'
            def getheaders(self):
                return [('content-type', self.getheader('content-type'))]
            def read1(self, size):
                if self.sent:
                    return b''
                self.sent = True
                response = {'object': 'response', 'id': 'resp_fixture', 'model': 'fixture-model', 'status': 'completed', 'usage': {'input_tokens': 3, 'output_tokens': 2}}
                if sse:
                    return b'data: ' + json.dumps({'type': 'response.completed', 'response': response}).encode() + b'\n\n'
                return json.dumps(response).encode()
        class Connection:
            def connect(self):
                pass
            def request(self, *args, **kwargs):
                ledger, _ = checkpoint_meter.snapshot(directory, 'live')
                assert ledger['active'] == 1 and ledger['completed'] == 0
            def getresponse(self):
                return Response()
            def close(self):
                pass
        class Handler(meter.Meter):
            runs = base
            incarnation = 'live'
            keys = {'openai': 'mock-no-credential'}
            clients = None
        checkpoint_meter.finish = paused_finish
        meter.upstream = lambda origin: Connection()
        server = meter.Server(('127.0.0.1', 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        errors = []
        def client():
            try:
                connection = http.client.HTTPConnection(*server.server_address, timeout=TIMEOUT_SECONDS)
                connection.request('POST', '/run/' + run_id + '/openai/v1/responses', body=b'{"model":"fixture-model"}')
                response = connection.getresponse()
                if sse:
                    assert b'response.completed' in response.readline()
                    consumed.set()
                    assert release_finish.wait(TIMEOUT_SECONDS)
                assert response.read()
                consumed.set()
                connection.close()
            except BaseException as error:
                errors.append(repr(error))
        reader = threading.Thread(target=client)
        reader.start()
        assert entered.wait(TIMEOUT_SECONDS)
        assert not consumed.wait(POLL_SECONDS)
        assert checkpoint_meter.snapshot(directory, 'live')[0]['active'] == 1
        drained = []
        barrier = threading.Thread(target=lambda: drained.append(checkpoint_meter.reconcile(directory, 'live', TIMEOUT_SECONDS)))
        barrier.start()
        wait_for(os.path.join(directory, 'meter-closed.json'))
        assert not drained
        release_finish.set()
        reader.join(TIMEOUT_SECONDS)
        barrier.join(TIMEOUT_SECONDS)
        assert consumed.is_set() and not errors and drained[0]['unknown'] == 0
        assert run.meter_totals(directory)['input_tokens'] == 3
        assert run.meter_totals(directory)['output_tokens'] == 2
        os.unlink(os.path.join(directory, 'requests.jsonl'))
        assert run.meter_totals(directory)['input_tokens'] == 3
        try:
            checkpoint_meter.begin(directory, 'live')
        except checkpoint.ResumeError:
            pass
        else:
            raise AssertionError('request accepted after finalization barrier')
        checkpoint_meter.finish, meter.upstream = original_finish, original_upstream
        server.shutdown()
        server.server_close()
        worker.join(TIMEOUT_SECONDS)
        results.append({'intent_before_upstream': True, 'eof_after_receipt': True, 'barrier_waited': True, 'receipt_without_log_counted': True, 'terminal_sse_consumed_before_receipt': False, 'terminal_sse_after_receipt': sse})
    return results


def intent_crash_drill(base):
    results = []
    for index in range(3):
        directory = os.path.join(base, 'intent-crash-' + str(index))
        checkpoint.durable_directory(directory)
        process = subprocess.Popen([sys.executable, os.path.abspath(__file__), '--intent-worker', directory])
        wait_for(os.path.join(directory, 'intent.active'))
        process.kill()
        process.wait()
        status = checkpoint_meter.reconcile(directory, 'replacement', 0)
        assert status['unknown'] == 1 and status['completed'] == 0
        again = checkpoint_meter.reconcile(directory, 'replacement', 0)
        assert again == status
        results.append(status)
    return results


def saved_agent_drill(base, original_args):
    results = []
    original_verify = run.verify
    for index in range(3):
        args = copy.copy(original_args)
        args.out = os.path.join(base, 'saved-agent-' + str(index))
        args.meter_incarnation = 'replacement'
        rows = run.matrix(args, ['hl-agent'])
        row = rows[0]
        args.campaign_identity = 'mock-saved-agent'
        directory = os.path.join(args.out, 'rows', row['id'])
        state = checkpoint.Attempt(directory, {'campaign': args.campaign_identity, 'row': row['id']})
        state.value.update(image_id=args.image_ids[row['id']], workdir='/app', accounting_status='observed', meter_incarnation='dead')
        state.save()
        journal = os.path.join(directory, 'journal')
        name = 'nanoeval-' + hashlib.sha256(os.path.join(args.out, 'rows').encode()).hexdigest()[:12] + '-' + row['id']
        run.prepare_container(row, args, {}, args.agent_root, journal, name, state)
        state.phase('agent', TIMEOUT_SECONDS)
        state.value['agent_result'] = {'exit': 0, 'timed_out': False, 'wall_ms': 1}
        state.save()
        checkpoint_meter.begin(directory, 'dead')
        calls = []
        def verify(*arguments):
            calls.append('same-container-verifier')
            return {'reward': 1, 'timed_out': False, 'exit': 0}
        run.verify = verify
        result = run.run_row(row, args, '127.0.0.1', {}, threading.Lock(), args.agent_root, os.path.join(args.out, 'rows'))
        assert result['meter']['unknown_requests'] is None
        assert result['meter']['durable_ledger']['unknown'] == 1
        assert calls == ['same-container-verifier']
        assert not os.path.exists(os.path.join(directory, 'agent.dispatches'))
        results.append({'saved_agent_not_replayed': True, 'unresolved_usage_unknown': True})
    run.verify = original_verify
    return results


def collision_drill(base):
    import socket
    results = []
    for index in range(3):
        directory = os.path.join(base, 'collision-' + str(index))
        checkpoint.durable_directory(directory)
        checkpoint.durable_directory(os.path.join(directory, 'rows'))
        checkpoint.atomic(os.path.join(directory, 'campaign.json'), {'identity': {'campaign': index}})
        with socket.socket() as free:
            free.bind(('127.0.0.1', 0))
            relay_port = free.getsockname()[1]
        foreign_command = [sys.executable, os.path.join(run.ROOT, 'checkpoint_meter.py'), '--role', 'relay', '--owner', 'foreign-campaign', '--environment-sha256', 'mock', '--state', os.path.join(directory, 'foreign.json'), '127.0.0.1', str(relay_port), '127.0.0.1:9']
        with open(os.path.join(directory, 'foreign.log'), 'a') as log:
            foreign = checkpoint_phase.service(foreign_command, log, dict(os.environ), ('127.0.0.1', relay_port))
        for _ in range(int(TIMEOUT_SECONDS / POLL_SECONDS)):
            try:
                socket.create_connection(('127.0.0.1', relay_port), timeout=1).close()
                break
            except OSError:
                time.sleep(POLL_SECONDS)
        else:
            raise AssertionError('foreign fixture relay did not start')
        with socket.socket() as free:
            free.bind(('127.0.0.1', 0))
            meter_port = free.getsockname()[1]
        try:
            run.start_services('127.0.0.1', '127.0.0.1:9', os.path.join(directory, 'rows'), directory, meter_port, relay_port, '127.0.0.1')
        except checkpoint.ResumeError as error:
            assert 'foreign or unowned' in str(error)
        else:
            raise AssertionError('foreign relay adopted')
        with socket.socket() as probe:
            assert probe.connect_ex(('127.0.0.1', relay_port)) == 0
        assert foreign.poll() is None
        state_path = os.path.join(directory, 'services', 'meter.json')
        if os.path.exists(state_path):
            pid = checkpoint.read(state_path)['pid']
            for _ in range(int(TIMEOUT_SECONDS / POLL_SECONDS)):
                try:
                    command = open('/proc/' + str(pid) + '/cmdline', 'rb').read()
                except FileNotFoundError:
                    break
                if not command:
                    break
                time.sleep(POLL_SECONDS)
            else:
                raise AssertionError('meter leaked after relay startup failure')
        checkpoint_phase.stop_services([foreign])
        results.append({'foreign_listener_preserved': True, 'started_meter_cleaned': True})
    return results


def cache_hook_drill(original_args):
    original_sh, original_module = run.sh, sys.modules.get('cache_store')
    args = copy.copy(original_args)
    rows = run.matrix(args, ['hl-agent', 'hl-agent-medium'])
    key = rows[0]['source'] + '/' + rows[0]['task']
    args.cache_variant = [key + '=' + 'a' * 64]
    manifest = {'warm_image_id': 'sha256:warm', 'base_image_id': 'sha256:mock-image'}
    calls = []
    def load(store, variant, expected):
        assert variant == 'a' * 64
        assert expected['base_image_id'] == 'sha256:mock-image'
        assert expected['task_digest'] == run.task_digest(rows[0]['dir'])
        assert expected['verifier_sha256'] == run.provenance.file_digest(os.path.join(rows[0]['dir'], 'tests', 'test.sh'))
        calls.append('full-artifact-audit')
        return manifest
    sys.modules['cache_store'] = SimpleNamespace(Store=lambda: None, load_variant=load, image_for_variant=lambda value: value['warm_image_id'])
    run.sh = lambda *args, **kwargs: SimpleNamespace(stdout=json.dumps([{'Id': 'sha256:warm', 'Config': {'WorkingDir': '/warm'}}]))
    images = {row['id']: 'sha256:mock-image' for row in rows}
    workdirs = {row['id']: '/app' for row in rows}
    run.cache_variants(args, rows, images, workdirs)
    assert calls == ['full-artifact-audit']
    assert set(images.values()) == {'sha256:warm'} and set(workdirs.values()) == {'/warm'}
    assert args.cache_identity['variants'][key]['manifest'] == manifest
    assert args.cache_identity['new_infrastructure_campaign'] is True
    rejected = 0
    for specifications in ([key + '=a', key + '=b'], ['unknown/task=a'], ['malformed']):
        args.cache_variant = specifications
        try:
            run.cache_variants(args, rows, images, workdirs)
        except checkpoint.ResumeError:
            rejected += 1
        else:
            raise AssertionError('invalid cache variant selection accepted')
    run.sh = original_sh
    if original_module is None:
        del sys.modules['cache_store']
    else:
        sys.modules['cache_store'] = original_module
    return {'all_arm_rows_pinned': len(rows), 'full_audits': len(calls), 'selection_mutations_rejected': rejected}


def ledger_mutation_drill(base):
    directory = os.path.join(base, 'ledger-mutations')
    checkpoint.durable_directory(directory)
    request = checkpoint_meter.begin(directory, 'original')
    record = {'request_id': request, 'meter_incarnation': 'original', 'path': '/responses', 'status': 200,
        'usage_known': True, 'usage': {'input_tokens': 7, 'output_tokens': 4}}
    checkpoint_meter.finish(directory, request, record)
    status = checkpoint_meter.reconcile(directory, 'replacement', 0)
    assert status['completed'] == 1 and status['unknown'] == 0
    assert run.meter_totals(directory)['input_tokens'] == 7
    receipt = os.path.join(directory, 'meter-intents', request + '.done.json')
    original = checkpoint.read(receipt)
    changed = copy.deepcopy(original)
    changed['record']['usage']['input_tokens'] = 0
    checkpoint.atomic(receipt, changed)
    assert checkpoint_meter.reconcile(directory, 'replacement', 0)['unknown'] == 1
    checkpoint.atomic(receipt, original)
    with open(os.path.join(directory, 'requests.jsonl'), 'w') as handle:
        handle.write(json.dumps(record) + '\n' + json.dumps(record) + '\n')
    assert checkpoint_meter.reconcile(directory, 'replacement', 0)['unmatched_records'] == 1
    with open(os.path.join(directory, 'requests.jsonl'), 'w') as handle:
        handle.write(json.dumps({'path': '/responses', 'status': 200, 'usage': {}}) + '\n')
    assert checkpoint_meter.reconcile(directory, 'replacement', 0)['unmatched_records'] == 1
    assert run.meter_totals(directory)['unknown_usage_requests'] == 1
    with open(os.path.join(directory, 'requests.jsonl'), 'w') as handle:
        handle.write('{"interrupted":')
    assert checkpoint_meter.reconcile(directory, 'replacement', 0)['unmatched_records'] == 1
    return {'valid_receipt_after_meter_restart': True, 'unknown_mutations': ['usage-digest', 'duplicate-audit', 'legacy-no-intent', 'torn-audit']}


def stream_usage_drill(base):
    import http.client
    import meter
    original_upstream = meter.upstream
    results = []
    for index in range(6):
        directory = os.path.join(base, 'truncated-sse-' + str(index))
        checkpoint.durable_directory(directory)
        class Response:
            status = 200
            sent = False
            def getheader(self, key):
                return 'application/octet-stream'
            def getheaders(self):
                return [('content-type', 'application/octet-stream')]
            def read1(self, size):
                if self.sent:
                    return b''
                self.sent = True
                if index >= 3:
                    return json.dumps({'object': 'chat.completion', 'usage': {'prompt_tokens': 7, 'completion_tokens': 4}}).encode()
                return b'data: ' + json.dumps({'type': 'message_start', 'message': {'usage': {'input_tokens': 7, 'output_tokens': 0}}}).encode() + b'\n\n'
        class Connection:
            def connect(self):
                pass
            def request(self, *args, **kwargs):
                pass
            def getresponse(self):
                return Response()
            def close(self):
                pass
        class Handler(meter.Meter):
            runs = base
            incarnation = 'live'
            keys = {'openai': 'mock-no-credential'}
            clients = None
        meter.upstream = lambda origin: Connection()
        server = meter.Server(('127.0.0.1', 0), Handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        connection = http.client.HTTPConnection(*server.server_address, timeout=TIMEOUT_SECONDS)
        endpoint = '/chat/completions' if index >= 3 else '/responses'
        connection.request('POST', '/run/truncated-sse-' + str(index) + '/openai/v1' + endpoint, body=b'{}')
        assert connection.getresponse().read()
        connection.close()
        assert run.meter_totals(directory)['unknown_usage_requests'] == 1
        ledger, records = checkpoint_meter.snapshot(directory, 'live')
        assert ledger['completed'] == 1 and not records[0]['usage_known']
        server.shutdown()
        server.server_close()
        worker.join(TIMEOUT_SECONDS)
        results.append({'nonterminal_mislabeled_sse_unknown': index < 3, 'unsupported_chat_usage_unknown': index >= 3})
    meter.upstream = original_upstream
    return results


def routing_drill():
    import meter
    paths = []
    for endpoint in ('/messages', '/responses', '/responses/compact', '/chat/completions'):
        provider, path = meter.route('chatgpt/v1' + endpoint)
        assert provider == 'chatgpt'
        forwarded, translated = meter.platform_models(path)
        assert not translated and forwarded == path
        assert path.endswith(checkpoint_meter.MODEL_ENDPOINTS)
        paths.append(path)
    _, models = meter.route('chatgpt/v1/models')
    _, translated = meter.platform_models(models)
    assert translated and not models.endswith(checkpoint_meter.MODEL_ENDPOINTS)
    return {'billed_routes_not_model_list_translation': paths, 'translated_models_get_unbilled': True}


def source_pin_drill(base):
    import provenance
    root = os.path.join(base, 'source-pin-fixture')
    paths = ['hosts/runtime' + suffix for suffix in ('.wat', '.mjs', '.js', '.c', '.h', '.inc')]
    paths += ['rust-toolchain', 'rust-toolchain.toml', '.cargo/config.toml', '.cargo/config']
    excluded = ['agents/' + name + '/artifact.py' for name in provenance.SOURCE_EXCLUDED]
    for path in paths + excluded:
        absolute = os.path.join(root, path)
        os.makedirs(os.path.dirname(absolute), exist_ok=True)
        with open(absolute, 'w') as handle:
            handle.write('fixture input')
    pins = provenance.source_files(root)
    assert set(pins) == set(paths)
    campaign_path = os.path.join(base, 'source-pin-campaign')
    campaign = checkpoint.Campaign(campaign_path, {'source_files': pins}, False)
    campaign.close()
    for path in paths:
        absolute = os.path.join(root, path)
        with open(absolute, 'w') as handle:
            handle.write('mutated input')
        mutated = provenance.source_files(root)
        assert mutated[path] != pins[path]
        try:
            checkpoint.Campaign(campaign_path, {'source_files': mutated}, True)
        except checkpoint.ResumeError:
            pass
        else:
            raise AssertionError('runtime/toolchain mutation accepted: ' + path)
        with open(absolute, 'w') as handle:
            handle.write('fixture input')
    for path in excluded:
        with open(os.path.join(root, path), 'w') as handle:
            handle.write('changed artifact')
    assert provenance.source_files(root) == pins
    return {'runtime_toolchain_mutations_rejected': len(paths), 'artifact_directories_ignored': len(excluded)}


TIMEOUT_CASES = ('closed', 'inflight', 'unknown-usage', 'infrastructure', 'interruption',
    'incarnation', 'unmatched', 'unproven-dispatch', 'lost-usage', 'legacy-weak', 'no-timeout', 'verifier-timeout')
TIMEOUT_REPETITIONS = 3
TIMEOUT_BUDGET_SEC = 0.05


def timeout_request(directory, known=True, pending=False):
    model, started = 'gpt-6-luna', time.time()
    metadata = dict(schema=3, provider='openai', path='/responses', model=model, started=started,
        body_sha256=checkpoint.digest({'model': model}),
        attempt_identity_sha256=checkpoint_meter.attempt_identity_sha256(directory, os.path.basename(directory)))
    request_id = checkpoint_meter.begin(directory, 'timeout-drill', metadata)
    if pending:
        return request_id
    intent = checkpoint.read(os.path.join(directory, 'meter-intents', request_id + '.intent.json'))
    usage = {'input_tokens': 7, 'output_tokens': 4, 'cache_read_tokens': 0, 'cache_write_tokens': 0} if known else None
    terminal = dict(complete=known, identity_matches=known, model=model,
        response_sha256=checkpoint.digest({'fixture': request_id}), provider_response_id='response-' + request_id,
        usage_sha256=checkpoint.digest(usage))
    record = dict(run=os.path.basename(directory), request_id=request_id, meter_incarnation='timeout-drill',
        provider='openai', path='/responses', started=started, ended=time.time(), request={'model': model},
        intent_sha256=checkpoint.digest(intent), status=200 if known else 0,
        usage_known=known, usage=usage, terminal=terminal)
    checkpoint_meter.finish(directory, request_id, record)
    return request_id


def timeout_fixture(base, case, index):
    directory = os.path.join(base, 'timeout-' + case + '-' + str(index))
    task, root = os.path.join(directory, 'task'), os.path.join(directory, 'agent-root')
    checkpoint.durable_directory(os.path.join(task, 'tests'))
    checkpoint.durable_directory(root)
    with open(os.path.join(task, 'task.toml'), 'w') as handle:
        agent_budget = 1 if case in ('no-timeout', 'verifier-timeout') else TIMEOUT_BUDGET_SEC
        verifier_budget = TIMEOUT_BUDGET_SEC if case == 'verifier-timeout' else 1
        handle.write('[environment]\ndocker_image="mock:1"\n[agent]\ntimeout_sec=' + str(agent_budget)
            + '\n[verifier]\ntimeout_sec=' + str(verifier_budget) + '\n')
    for path in ('instruction.md', 'tests/test.sh'):
        with open(os.path.join(task, path), 'w') as handle:
            handle.write('offline timeout fixture\n')
    row = dict(id='row', source='mock', task='timeout', agent='hl-agent', attempt=1, dir=task)
    args = SimpleNamespace(campaign_identity='timeout-fixture', model='gpt-6-luna', limits='none',
        agent_memory_mb=0, max_turns=None, ca_bundle=None, meter_host=None, meter_port=1,
        openai_route='openai', hl_agent_arg=[], meter_incarnation='timeout-drill', agent_root=root,
        image_ids={'row': 'sha256:mock-image'}, workdirs={'row': '/app'})
    runs = os.path.join(directory, 'rows')
    state = checkpoint.Attempt(os.path.join(runs, row['id']), dict(campaign=args.campaign_identity, row=row['id']))
    status = {'infrastructure': 'unknown_after_infrastructure', 'interruption': 'unknown_after_interruption',
        'lost-usage': 'unknown_model_usage'}.get(case, 'observed')
    state.value.update(image_id=args.image_ids['row'], workdir='/app', accounting_status=status,
        meter_incarnation='prior' if case == 'incarnation' else args.meter_incarnation)
    state.save()
    return directory, row, args, runs


def timeout_legacy_receipt(directory, request_id):
    root = os.path.join(directory, 'meter-intents', request_id)
    intent = checkpoint.read(root + '.intent.json')
    intent.pop('schema')
    checkpoint.atomic(root + '.intent.json', intent)
    done = checkpoint.read(root + '.done.json')
    done['record'].pop('intent_sha256')
    done['record'].pop('terminal')
    done['record_sha256'] = checkpoint.digest(done['record'])
    checkpoint.atomic(root + '.done.json', done)


def timeout_case(base, case, index):
    directory, row, args, runs = timeout_fixture(base, case, index)
    out = os.path.join(runs, row['id'])
    commands, request_ids = [], []
    original_sh, original_launch, original_reconcile = run.sh, checkpoint_phase.launch, checkpoint_meter.reconcile
    def shell(command, **kwargs):
        commands.append(command)
        return mock_sh(directory, command, **kwargs)
    def launched(sh, name, row, env, argv, stage, token):
        if stage == 'agent':
            request_ids.append(timeout_request(out))
            if case in ('inflight', 'unknown-usage', 'unproven-dispatch'):
                request_ids.append(timeout_request(out, known=False, pending=case == 'inflight'))
            if case == 'legacy-weak':
                timeout_legacy_receipt(out, request_ids[0])
            if case == 'unmatched':
                with open(os.path.join(out, 'requests.jsonl'), 'w') as handle:
                    handle.write(json.dumps({'path': '/unrecognized', 'request_id': 'orphan'}) + '\n')
        timed_out = stage == ('verifier' if case == 'verifier-timeout' else 'agent') and case != 'no-timeout'
        if not timed_out:
            checkpoint.atomic(os.path.join(out, 'journal', stage + '.receipt.json'), {'exit': 0, 'token': token})
    run.sh, checkpoint_phase.launch = shell, launched
    checkpoint_meter.reconcile = lambda directory, incarnation: original_reconcile(directory, incarnation, 0)
    try:
        result = run.run_row(row, args, '127.0.0.1', {}, threading.Lock(), args.agent_root, runs)
        again = run.run_row(row, args, '127.0.0.1', {}, threading.Lock(), args.agent_root, runs)
        assert again == result
    finally:
        run.sh, checkpoint_phase.launch, checkpoint_meter.reconcile = original_sh, original_launch, original_reconcile
    meter = result['meter']
    assert result['reporting_rules'] == run.reporting_rules.pin()
    expected = {'closed': 'observed_after_timeout_reconciliation', 'verifier-timeout': 'observed_after_timeout_reconciliation',
        'infrastructure': 'unknown_after_infrastructure', 'interruption': 'unknown_after_interruption',
        'incarnation': 'unknown_meter_incarnation_changed', 'lost-usage': 'unknown_model_usage',
        'no-timeout': 'observed', 'unknown-usage': 'unknown_model_usage',
        'unproven-dispatch': 'unknown_model_usage'}.get(case, 'unknown_meter_requests')
    assert result['accounting_status'] == meter['accounting_status'] == expected, result
    assert result['outcome'] == ('passed' if case == 'no-timeout' else 'errored' if case == 'verifier-timeout' else 'failed')
    assert result['agent_result']['timed_out'] is (case not in ('no-timeout', 'verifier-timeout'))
    assert result['verifier']['timed_out'] is (case == 'verifier-timeout')
    assert meter['input_tokens'] == 7 and meter['output_tokens'] == 4
    assert meter['unknown_requests'] == (0 if case in ('closed', 'verifier-timeout', 'no-timeout') else None)
    assert meter['unproven_intents'] == int(case in ('legacy-weak', 'unknown-usage', 'unproven-dispatch'))
    assert meter['durable_ledger']['closed'] is True
    assert meter['durable_ledger']['unknown'] == int(case == 'inflight')
    assert meter['unknown_usage_requests'] == int(case in ('unknown-usage', 'unproven-dispatch'))
    assert sorted(path.removesuffix('.intent.json') for path in os.listdir(os.path.join(out, 'meter-intents'))
        if path.endswith('.intent.json')) == sorted(request_ids)
    stops = [command for command in commands if command[:2] == ['docker', 'stop']]
    starts = [command for command in commands if command[:2] == ['docker', 'start']]
    assert len(stops) == (1 if case == 'no-timeout' else 2)
    assert len(starts) == int(case not in ('no-timeout', 'verifier-timeout'))
    return dict(case=case, repetition=index, directory=out, accounting_status=expected,
        outcome=result['outcome'], request_ids=request_ids, actual_deadline_expired=case != 'no-timeout')


def timeout_proof_mutations():
    original = dict(closed=True, intents=1, completed=1, unknown=0, unmatched_records=0)
    mutations = dict(open=dict(closed=False), missing_closed=dict(closed=None), active=dict(active=1),
        unknown=dict(unknown=1), unmatched=dict(unmatched_records=1), incomplete=dict(completed=0),
        malformed_count=dict(completed=True), missing_unknown=dict(unknown=None), missing_unmatched=dict(unmatched_records=None))
    for name, mutation in mutations.items():
        state = SimpleNamespace(value={'accounting_status': 'unknown_at_timeout'})
        totals = {'unknown_usage_requests': 0, 'unproven_intents': 0}
        run.reconcile_accounting(state, dict(original, **mutation), totals)
        assert state.value['accounting_status'] == 'unknown_meter_requests', name
        assert totals['unknown_requests'] is None, name
    taints = ('unknown_after_infrastructure', 'unknown_after_interruption', 'unknown_meter_incarnation_changed',
        'unknown_meter_requests', 'unknown_model_usage')
    for taint in taints:
        state = SimpleNamespace(value={'accounting_status': taint})
        totals = {'unknown_usage_requests': 0, 'unproven_intents': 0}
        run.reconcile_accounting(state, original, totals)
        assert state.value['accounting_status'] == taint
        assert totals['unknown_requests'] is None
    return dict(incomplete_proofs_rejected=len(mutations), sticky_taints=len(taints))


def timeout_drill(base):
    results = [timeout_case(base, case, index) for case in TIMEOUT_CASES for index in range(TIMEOUT_REPETITIONS)]
    return dict(result='passed', cases=results, rows=len(results), repetitions=TIMEOUT_REPETITIONS,
        proof_mutations=[timeout_proof_mutations() for _ in range(TIMEOUT_REPETITIONS)],
        model_calls=0, docker_calls=0)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--intent-worker')
    parser.add_argument('--child', nargs=2)
    parser.add_argument('--phase-worker', nargs=3)
    parser.add_argument('--out')
    parser.add_argument('--timeout-only', action='store_true')
    parser.add_argument('--service-owner')
    parser.add_argument('--service-worker')
    args = parser.parse_args()
    if args.intent_worker:
        checkpoint_meter.begin(args.intent_worker, 'dead')
        checkpoint.atomic(os.path.join(args.intent_worker, 'intent.active'), True)
        wait_for(os.path.join(args.intent_worker, 'never.release'))
        return
    if args.service_owner:
        service_owner(args.service_owner)
        return
    if args.service_worker:
        wait_for(os.path.join(args.service_worker, "service.release"))
        return
    if args.child:
        child(args.child[0], int(args.child[1]))
        return
    if args.phase_worker:
        phase_worker(*args.phase_worker)
        return
    if args.out:
        checkpoint.durable_directory(os.path.abspath(args.out))
    base = tempfile.mkdtemp(prefix='checkpoint-drill-', dir=args.out)
    if args.timeout_only:
        result = timeout_drill(base)
        checkpoint.atomic(os.path.join(base, 'timeout-result.json'), result)
        print(json.dumps(dict(result, fixture=base), sort_keys=True))
        return
    fixture, rows, identity = resume_drill(base)
    rejected = mutation_drill(fixture, rows, identity)
    interruptions = interruption_drill(base)
    shell_receipts = shell_receipt_drill(base)
    service_recoveries = service_drill(base)
    stopped = stopped_drill(base, fixture)
    accounting = accounting_drill(base)
    terminal_sse = accounting_drill(base, True)
    source_pins = source_pin_drill(base)
    intents = intent_crash_drill(base)
    saved = saved_agent_drill(base, fixture)
    collisions = collision_drill(base)
    cache = cache_hook_drill(fixture)
    ledger_mutations = ledger_mutation_drill(base)
    streams = stream_usage_drill(base)
    routing = routing_drill()
    timeouts = timeout_drill(base)
    print(json.dumps({'fixture': base, 'matrix_rows': len(rows), 'process_kills': 3,
        'agent_dispatches': 3, 'verifier_dispatches': 3, 'completed_immutable': 3,
        'identity_mutations_rejected': rejected, 'interruption_guards': interruptions,
        'timeout_accounting_n3': timeouts, 'terminal_sse_barrier_n3': terminal_sse, 'runtime_source_pins': source_pins, 'api_edge_n3_each': streams, 'routing': routing, 'ledger_mutations': ledger_mutations, 'cache_hook': cache, 'accounting_n3': accounting, 'intent_owner_sigkill_n3': intents, 'saved_agent_window_n3': saved, 'relay_collision_cleanup_n3': collisions,
        'shell_receipts': shell_receipts, 'service_owner_kill_recoveries': service_recoveries, 'stopped_container_drill': stopped, 'model_calls': 0, 'docker_calls': 0}, sort_keys=True))


if __name__ == '__main__':
    main()
