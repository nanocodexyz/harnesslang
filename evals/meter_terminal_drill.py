import argparse
import hashlib
import http.client
import io
import json
import os
from pathlib import Path
import signal
import socket
import ssl
import subprocess
import sys
import tempfile
import time

import checkpoint
import checkpoint_meter
import meter
import report

REPETITIONS = 3
WAIT_SECONDS = 10
POLL_SECONDS = 0.01
CASES = ('before-sse', 'after-sse', 'before-json', 'after-json', 'keyed-only',
    'disconnect', 'terminal-disconnect', 'truncated', 'id-mismatch', 'model-mismatch', 'malformed', 'deliver-sse', 'deliver-json',
    'after-anthropic', 'truncated-anthropic', 'incomplete', 'failed', 'missing-model', 'missing-id',
    'incomplete-missing-status', 'failed-missing-status', 'incomplete-completed-status', 'failed-completed-status')
DISPATCH_CASES = ('connect-oserror', 'connect-dns', 'connect-tls', 'before-connect',
    'request-same-error', 'partial-send', 'incomplete-read')
MODEL = 'gpt-6-luna'
BODY = json.dumps({'model': MODEL, 'stream': True, 'input': 'fixture-only'}).encode()


def wait_for(path):
    for _ in range(int(WAIT_SECONDS / POLL_SECONDS)):
        if path.exists():
            return
        time.sleep(POLL_SECONDS)
    raise RuntimeError('offline worker readiness deadline: ' + str(path))


def frame(event):
    return b'data: ' + json.dumps(event).encode() + b'\r\n\r\n'


def payload(case):
    response = {'object': 'response', 'id': 'resp_fixture', 'model': MODEL,
        'status': 'completed', 'usage': {'input_tokens': 7, 'output_tokens': 4}}
    if case.endswith('anthropic'):
        response = {'id': 'resp_fixture', 'model': MODEL, 'usage': {'input_tokens': 7, 'output_tokens': 0}}
        start = frame({'type': 'message_start', 'message': response})
        delta = frame({'type': 'message_delta', 'delta': {'stop_reason': 'end_turn'}, 'usage': {'output_tokens': 4}})
        return start + delta + (frame({'type': 'message_stop'}) if case == 'after-anthropic' else b'')
    if case.endswith('json'):
        return json.dumps(response).encode()
    if case.split('-')[0] in ('incomplete', 'failed'):
        kind = case.split('-')[0]
        response['status'] = kind
        response['error'] = None
        if case.endswith('missing-status'):
            del response['status']
        elif case.endswith('completed-status'):
            response['status'] = 'completed'
        return frame({'type': 'response.' + kind, 'response': response})
    if case in ('missing-model', 'missing-id'):
        del response['model' if case == 'missing-model' else 'id']
        return frame({'type': 'response.completed', 'response': response})
    created = frame({'type': 'response.created', 'response': {'id': 'resp_fixture', 'model': MODEL}})
    if case == 'truncated':
        return created + frame({'type': 'response.completed', 'response': response})[:-2]
    if case == 'id-mismatch':
        response['id'] = 'resp_wrong'
    if case == 'model-mismatch':
        response['model'] = 'gpt-6.1-terra'
    if case == 'malformed':
        return created + b'data: {not-json}\n\n'
    return created + frame({'type': 'response.completed', 'response': response})


def worker(directory, case):
    root = directory.parent
    wire = payload(case)
    if case == 'incomplete-read':
        wire = frame({'type': 'response.created', 'response': {'id': 'resp_fixture', 'model': MODEL}})
    response_type = 'application/json' if case.endswith('json') else 'text/event-stream'
    original_finish, original_atomic = checkpoint_meter.finish, checkpoint.atomic

    def pause():
        original_atomic(str(directory / 'ready.json'), {'case': case})
        for _ in range(int(WAIT_SECONDS / POLL_SECONDS)):
            if (directory / 'release').exists():
                return
            time.sleep(POLL_SECONDS)
        raise RuntimeError('parent did not kill worker')

    def finish(*args):
        if case.startswith('before-'):
            pause()
        original_finish(*args)
        pause()

    def atomic(path, *args, **kwargs):
        if case == 'keyed-only' and path.endswith('.done.json'):
            pause()
        return original_atomic(path, *args, **kwargs)

    class Response:
        status = 200

        def __init__(self):
            self.source = io.BytesIO(wire)

        def getheader(self, name):
            return response_type

        def getheaders(self):
            return [('content-type', response_type)]

        def read1(self, size):
            data = self.source.read(min(size, len(b'data: ')))
            if not data and case == 'incomplete-read':
                raise http.client.IncompleteRead(b'partial')
            return data

    class Connection:
        def connect(self):
            intents = list((directory / 'meter-intents').glob('*.intent.json'))
            assert len(intents) == 1 and not list((directory / 'meter-intents').glob('*.done.json'))
            with open(directory / 'connects', 'a') as handle:
                handle.write('connect\n')
            if case in ('connect-oserror', 'before-connect'):
                raise OSError('Tunnel connection failed: 502 Bad Gateway')
            if case == 'connect-dns':
                raise socket.gaierror('offline DNS failure')
            if case == 'connect-tls':
                raise ssl.SSLError('offline TLS failure')

        def request(self, method, path, body, headers):
            assert self.auto_open is False
            intents = list((directory / 'meter-intents').glob('*.intent.json'))
            assert len(intents) == 1
            intent = checkpoint.read(str(intents[0]))
            assert intent['provider'] == 'openai' and intent['model'] == MODEL
            assert intent['path'] == '/v1/responses'
            assert intent['body_sha256'] == hashlib.sha256(BODY).hexdigest()
            assert intent['attempt_identity_sha256'] == checkpoint.digest({'fixture': directory.name})
            assert intent['started'] > 0 and intent['incarnation'] == 'original'
            assert 'do-not-persist' not in json.dumps(intent)
            with open(directory / 'dispatches', 'a') as handle:
                handle.write('dispatch\n')
            if case == 'partial-send':
                class PartialSocket:
                    def sendall(self, data):
                        (directory / 'partial-bytes').write_bytes(data[:len(data) // 2])
                        raise OSError('Tunnel connection failed: 502 Bad Gateway')
                transport = http.client.HTTPConnection('offline.invalid')
                transport.sock, transport.auto_open = PartialSocket(), False
                transport.request(method, path, body=body, headers=headers)
                raise AssertionError('partial send did not fail')
            if case == 'request-same-error':
                raise OSError('Tunnel connection failed: 502 Bad Gateway')

        def getresponse(self):
            return Response()

        def close(self):
            pass

    class Client:
        def write(self, data):
            terminal_frame = b'response.completed' in data
            final_write = data == b'0\r\n\r\n'
            if case == 'disconnect' or (case == 'terminal-disconnect' and (terminal_frame or final_write)):
                if terminal_frame or final_write:
                    assert len(list((directory / 'meter-intents').glob('*.done.json'))) == 1
                    original_atomic(str(directory / ('final-write-failed.json' if final_write else 'terminal-write-failed.json')), True)
                raise BrokenPipeError('offline downstream disconnect')
            with open(directory / 'client.bin', 'ab') as handle:
                handle.write(data)
                handle.flush()
                os.fsync(handle.fileno())

        def flush(self):
            pass

    handler = object.__new__(meter.Meter)
    handler.runs, handler.keys, handler.incarnation = str(root), {'openai': 'offline-fixture'}, 'original'
    handler.clients, handler.command = None, 'POST'
    handler.path = '/run/' + directory.name + '/openai/v1/responses?secret=do-not-persist'
    handler.headers = {'authorization': 'Bearer do-not-persist', 'x-private': 'do-not-persist'}
    handler.send_response = handler.send_header = handler.end_headers = lambda *args: None
    handler.wfile, handler.close_connection = Client(), False
    meter.upstream = lambda origin: Connection()
    checkpoint_meter.finish, checkpoint.atomic = finish, atomic
    handler.forward(BODY)
    original_atomic(str(directory / 'delivered.json'), True)
    time.sleep(WAIT_SECONDS)


def check_case(base, case, index):
    directory = base / (case + '-' + str(index))
    directory.mkdir()
    checkpoint.atomic(str(directory / 'checkpoint.json'), {'identity': {'fixture': directory.name}})
    with open(directory / 'worker.log', 'wb') as log:
        process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
            '--worker', str(directory), '--case', case], stdout=log, stderr=log,
            env=dict(os.environ, PYTHONDONTWRITEBYTECODE='1'))
        try:
            wait_for(directory / 'ready.json')
            client = (directory / 'client.bin').read_bytes() if (directory / 'client.bin').exists() else b''
            assert b'response.completed' not in client
            assert b'"object": "response"' not in client
            if case.startswith('deliver-') or case in ('disconnect', 'terminal-disconnect'):
                (directory / 'release').touch()
                wait_for(directory / 'delivered.json')
                client = (directory / 'client.bin').read_bytes() if (directory / 'client.bin').exists() else b''
                if case.startswith('deliver-'):
                    assert (b'response.completed' in client) if case.endswith('sse') else (b'"object": "response"' in client)
                else:
                    assert (directory / 'final-write-failed.json').exists()
                    if case == 'terminal-disconnect':
                        assert (directory / 'terminal-write-failed.json').exists()
                assert len(list((directory / 'meter-intents').glob('*.done.json'))) == 1
                (directory / 'requests.jsonl').unlink()
                (directory / 'responses.jsonl').unlink()
            process.kill()
            assert process.wait(timeout=WAIT_SECONDS) == -signal.SIGKILL
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=WAIT_SECONDS)
    status = checkpoint_meter.reconcile(str(directory), 'replacement', 0)
    snapshot, records = checkpoint_meter.snapshot(str(directory), 'replacement')
    unresolved = case.startswith('before-') or case == 'keyed-only'
    known = case in ('after-sse', 'after-json', 'disconnect', 'terminal-disconnect', 'deliver-sse', 'deliver-json', 'after-anthropic')
    assert status['unknown'] == int(unresolved)
    assert snapshot['completed'] == int(not unresolved)
    assert not records if unresolved else records[0]['usage_known'] is known
    row = {'id': directory.name, 'meter': {'requests': 1, 'unknown_requests': int(unresolved)}}
    cost = report.price_row(directory, row, True)
    assert (cost['total_usd'] is not None) is known
    assert cost['ledger']['completed'] == int(not unresolved)
    assert not (directory / 'requests.jsonl').exists()
    if known:
        record = records[0]
        terminal = record['terminal']
        assert terminal['provider_response_id'] == 'resp_fixture'
        assert terminal['model'] == MODEL
        assert terminal['usage_sha256'] == checkpoint.digest(record['usage'])
        assert terminal['response_sha256'] and terminal['envelope_sha256']
        assert record['usage']['input_tokens'] == 7 and record['usage']['output_tokens'] == 4
        assert len(list((directory / 'meter-receipts').glob('*.terminal.json'))) == 1
    if case == 'keyed-only':
        assert len(list((directory / 'meter-receipts').glob('*.terminal.json'))) == 1
    assert checkpoint_meter.reconcile(str(directory), 'replacement', 0) == status
    assert checkpoint_meter.snapshot(str(directory), 'replacement') == (snapshot, records)
    assert (directory / 'dispatches').read_text() == 'dispatch\n'
    for path in list((directory / 'meter-intents').glob('*.json')) + list((directory / 'meter-receipts').glob('*.json')):
        assert 'do-not-persist' not in path.read_text()
    return {'case': case, 'repetition': index, 'sigkill': True, 'usage_known': known,
        'unknown_intents': status['unknown'], 'final_write_failed': (directory / 'final-write-failed.json').exists(), 'report_total_known': cost['total_usd'] is not None}


def mutation_check(base):
    directory = base / 'after-sse-0'
    done = next((directory / 'meter-intents').glob('*.done.json'))
    original = checkpoint.read(str(done))
    mutations = ('request_id', 'meter_incarnation', 'intent_sha256', 'model', 'usage', 'terminal')
    for field in mutations:
        value = json.loads(json.dumps(original))
        record = value['record']
        if field == 'model':
            record['request']['model'] = 'wrong'
        elif field == 'usage':
            record['usage']['input_tokens'] += 1
        elif field == 'terminal':
            record['terminal']['complete'] = False
        else:
            record[field] = 'wrong'
        value['record_sha256'] = checkpoint.digest(record)
        checkpoint.atomic(str(done), value)
        status, records = checkpoint_meter.snapshot(str(directory), 'replacement')
        assert status['unknown'] == 1 and not records
        cost = report.price_row(directory, {'id': directory.name, 'meter': {'requests': 1, 'unknown_requests': 0}}, True)
        assert cost['total_usd'] is None and cost['ledger']['completed'] == 0
    checkpoint.atomic(str(done), original)
    intent_path = next((directory / 'meter-intents').glob('*.intent.json'))
    intent = checkpoint.read(str(intent_path))
    intent_mutations = ('body_sha256', 'attempt_identity_sha256', 'schema-unknown', 'schema-removed')
    for field in intent_mutations:
        value = dict(intent)
        if field == 'schema-removed':
            value.pop('schema')
        else:
            value['schema' if field == 'schema-unknown' else field] = 'wrong'
        checkpoint.atomic(str(intent_path), value)
        status, records = checkpoint_meter.snapshot(str(directory), 'replacement')
        assert status['unknown'] == 1 and not records
        cost = report.price_row(directory, {'id': directory.name, 'meter': {'requests': 1, 'unknown_requests': 0}}, True)
        assert cost['total_usd'] is None and cost['ledger']['completed'] == 0
    checkpoint.atomic(str(intent_path), intent)
    legacy_intent = dict(intent, schema=2)
    legacy_receipt = json.loads(json.dumps(original))
    legacy_receipt['record']['intent_sha256'] = checkpoint.digest(legacy_intent)
    legacy_receipt['record_sha256'] = checkpoint.digest(legacy_receipt['record'])
    checkpoint.atomic(str(intent_path), legacy_intent)
    checkpoint.atomic(str(done), legacy_receipt)
    assert checkpoint_meter.snapshot(str(directory), 'replacement')[0]['completed'] == 1
    assert report.price_row(directory, {'id': directory.name, 'meter': {'requests': 1}}, True)['total_usd'] is not None
    checkpoint.atomic(str(intent_path), intent)
    checkpoint.atomic(str(done), original)
    return list(mutations + intent_mutations)



def check_dispatch_case(base, case, index):
    import run
    directory = base / (case + '-' + str(index))
    directory.mkdir()
    checkpoint.atomic(str(directory / 'checkpoint.json'), {'identity': {'fixture': directory.name}})
    with open(directory / 'worker.log', 'wb') as log:
        process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()),
            '--worker', str(directory), '--case', case], stdout=log, stderr=log,
            env=dict(os.environ, PYTHONDONTWRITEBYTECODE='1'))
        try:
            wait_for(directory / 'ready.json')
            process.kill()
            assert process.wait(timeout=WAIT_SECONDS) == -signal.SIGKILL
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=WAIT_SECONDS)
    status = checkpoint_meter.reconcile(str(directory), 'replacement', 0)
    _, records = checkpoint_meter.snapshot(str(directory), 'replacement')
    proven = case.startswith('connect-')
    unresolved = case == 'before-connect'
    totals = run.meter_totals(str(directory))
    assert status['unknown'] == int(unresolved)
    assert status['completed'] == int(not unresolved)
    assert totals['not_dispatched_requests'] == int(proven)
    assert totals['ok_requests'] == 0
    assert totals['unknown_usage_requests'] == int(not proven and not unresolved)
    assert run.outcome({}, {'reward': 1}, totals)[0] == 'errored'
    row = {'id': directory.name, 'meter': dict(totals, unknown_requests=int(unresolved))}
    priced = report.price_row(directory, row, True)
    assert priced['total_usd'] is None
    assert priced['not_dispatched_requests'] == int(proven)
    if proven:
        intent = checkpoint.read(str(next((directory / 'meter-intents').glob('*.intent.json'))))
        assert report.request_cost(records[0], None, directory.name, intent) == (0.0, None)
        assert not any(key in records[0] for key in ('usage', 'usage_known', 'terminal'))
    assert (directory / 'connects').read_text() == 'connect\n'
    dispatches = (directory / 'dispatches').read_text() if (directory / 'dispatches').exists() else ''
    assert dispatches == ('' if proven or unresolved else 'dispatch\n')
    assert (directory / 'partial-bytes').exists() is (case == 'partial-send')
    if case == 'partial-send':
        assert (directory / 'partial-bytes').read_bytes().startswith(b'POST ')
    return {'case': case, 'repetition': index, 'request_calls': int(bool(dispatches)),
        'not_dispatched': proven, 'unresolved_intent': unresolved, 'all_no_model_guard': True}


def dispatch_mutations(base, index):
    import run
    directory = base / ('connect-oserror-' + str(index))
    done = next((directory / 'meter-intents').glob('*.done.json'))
    intent_path = next((directory / 'meter-intents').glob('*.intent.json'))
    original, intent = checkpoint.read(str(done)), checkpoint.read(str(intent_path))
    fields = ('intent_sha256', 'request_id', 'meter_incarnation', 'usage', 'usage_known',
        'terminal', 'status', 'proof-schema', 'reason', 'body_sha256',
        'attempt_identity_sha256', 'incarnation', 'old-schema', 'legacy-schema')
    for field in fields:
        changed, changed_intent = json.loads(json.dumps(original)), dict(intent)
        record = changed['record']
        if field in ('body_sha256', 'attempt_identity_sha256', 'incarnation', 'old-schema', 'legacy-schema'):
            if field == 'legacy-schema':
                changed_intent.pop('schema')
            else:
                changed_intent['schema' if field == 'old-schema' else field] = 2 if field == 'old-schema' else 'f' * 64
            if field == 'attempt_identity_sha256':
                record['intent_sha256'] = checkpoint.digest(changed_intent)
        elif field == 'proof-schema':
            record['not_dispatched']['schema'] = True
        elif field == 'reason':
            record['not_dispatched']['reason'] = 'unknown'
        else:
            record[field] = 200 if field == 'status' else True if field == 'usage_known' else {} if field in ('usage', 'terminal') else 'wrong'
        changed['record_sha256'] = checkpoint.digest(record)
        checkpoint.atomic(str(done), changed)
        checkpoint.atomic(str(intent_path), changed_intent)
        status, records = checkpoint_meter.snapshot(str(directory), 'replacement')
        assert status['unknown'] == 1 and not records, field
        assert run.meter_totals(str(directory))['not_dispatched_requests'] == 0
        priced = report.price_row(directory, {'id': directory.name, 'meter': {'requests': 1}}, True)
        assert priced['total_usd'] is None and priced['not_dispatched_requests'] == 0
    checkpoint.atomic(str(done), original)
    checkpoint.atomic(str(intent_path), intent)
    return list(fields)


def dispatch_mixed(base, index):
    import run
    directory = base / ('mixed-' + str(index))
    directory.mkdir()
    root = directory / 'meter-intents'
    root.mkdir()
    for case in ('after-sse', 'connect-oserror', 'request-same-error'):
        source = base / (case + '-' + str(index))
        intent_path = next((source / 'meter-intents').glob('*.intent.json'))
        intent = checkpoint.read(str(intent_path))
        done_path = next((source / 'meter-intents').glob('*.done.json'))
        receipt = checkpoint.read(str(done_path))
        record = receipt['record']
        record['run'] = directory.name
        intent['attempt_identity_sha256'] = checkpoint.digest({'run': directory.name})
        record['intent_sha256'] = checkpoint.digest(intent)
        receipt['record_sha256'] = checkpoint.digest(record)
        checkpoint.atomic(str(root / intent_path.name), intent)
        checkpoint.atomic(str(root / done_path.name), receipt)
        checkpoint_meter.reconcile(str(directory), 'replacement', 0)
        totals = run.meter_totals(str(directory))
        priced = report.price_row(directory, {'id': directory.name, 'meter': totals}, True)
        assert totals['requests'] == (1 if case == 'after-sse' else 2 if case == 'connect-oserror' else 3)
        assert totals['ok_requests'] == 1
        assert totals['input_tokens'] == 7 and totals['output_tokens'] == 4
        expected = report.cost(MODEL, {'input_tokens': 7, 'output_tokens': 4,
            'cache_read_tokens': 0, 'cache_write_tokens': 0})
        assert priced['lower_bound_usd'] == expected
        assert (priced['total_usd'] == expected) is (case != 'request-same-error')
        if case != 'after-sse':
            assert totals['not_dispatched_requests'] == priced['not_dispatched_requests'] == 1
        if case == 'request-same-error':
            assert totals['unknown_usage_requests'] == 1 and priced['total_usd'] is None
        if case == 'connect-oserror':
            for taint in ('unknown_at_timeout', 'unknown_after_infrastructure'):
                tainted = report.price_row(directory, {'id': directory.name, 'meter': totals,
                    'accounting_status': taint}, True)
                assert tainted['total_usd'] is None
    return {'repetition': index, 'known_requests': 1, 'not_dispatched_requests': 1,
        'unknown_blocks_total': True, 'prior_taints_retained': True}


def timeout_accounting(base):
    import checkpoint_drill
    result = checkpoint_drill.timeout_drill(str(base))
    expected = report.cost(MODEL, {'input_tokens': 7, 'output_tokens': 4,
        'cache_read_tokens': 0, 'cache_write_tokens': 0})
    for case in result['cases']:
        directory = Path(case['directory'])
        row = checkpoint.read(str(directory / 'result.json'))
        priced = report.price_row(directory, row, True)
        assert priced['lower_bound_usd'] == expected, (case, priced)
        known = case['case'] in ('closed', 'verifier-timeout', 'no-timeout')
        assert priced['total_usd'] == (expected if known else None), (case, priced)
        assert priced['requests'] == row['meter']['requests']
        assert priced['not_dispatched_requests'] == 0
        case.update(lower_bound_usd=priced['lower_bound_usd'], total_usd=priced['total_usd'])
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--worker', type=Path)
    parser.add_argument('--case', choices=CASES + DISPATCH_CASES)
    parser.add_argument('--out', type=Path, default=Path('output/meter-terminal'))
    parser.add_argument('--timeout-only', action='store_true')
    args = parser.parse_args()
    if args.worker:
        worker(args.worker, args.case)
        return
    args.out.mkdir(parents=True, exist_ok=True)
    base = Path(tempfile.mkdtemp(prefix='drill-', dir=args.out)).resolve()
    if args.timeout_only:
        result = dict(timeout_accounting(base), fixture=str(base))
        checkpoint.atomic(str(base / 'result.json'), result)
        print(json.dumps(result, sort_keys=True))
        return
    results = [check_case(base, case, index) for case in CASES for index in range(REPETITIONS)]
    dispatch = [check_dispatch_case(base, case, index) for case in DISPATCH_CASES for index in range(REPETITIONS)]
    mixed = [dispatch_mixed(base, index) for index in range(REPETITIONS)]
    result = {'timeout_accounting_n3': timeout_accounting(base), 'dispatch_n3': dispatch, 'dispatch_mixed_n3': mixed,
        'dispatch_mutations_rejected_n3': [dispatch_mutations(base, index) for index in range(REPETITIONS)], 'result': 'passed', 'fixture': str(base), 'process_kills': len(results) + len(dispatch),
        'model_calls': 0, 'network_calls': 0, 'docker_calls': 0, 'results': results,
        'binding_mutations_rejected': mutation_check(base)}
    checkpoint.atomic(str(base / 'result.json'), result)
    print(json.dumps(result, sort_keys=True))


if __name__ == '__main__':
    main()
