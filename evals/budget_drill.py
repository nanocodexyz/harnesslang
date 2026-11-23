import concurrent.futures
import json
import io
import hashlib
from decimal import Decimal
import multiprocessing
import os
import tempfile
import threading

import budget
import meter

MODEL = 'gpt-6-luna'
PATH = '/v1/responses'
BODY = json.dumps({'model': MODEL, 'input': 'hello', 'max_output_tokens': 100}).encode()


def policy(cap='10'):
    evidence = {'source': 'offline fake provider hard bound, not production evidence',
                'path': os.path.abspath(__file__), 'sha256': hashlib.sha256(open(__file__, 'rb').read()).hexdigest()}
    bound = {'provider': 'openai', 'context_tokens_max': 1000, 'output_tokens_max': 100,
             'request_bytes_max': 100000, 'output_bound': 'request',
             'context_evidence': evidence, 'output_evidence': evidence}
    return {'schema': 1, 'cap_usd': cap, 'requests_max': 1000,
            'models': {MODEL: bound, 'gpt-6.1-terra': dict(bound)}}


def expect_error(action, text):
    try:
        action()
    except budget.BudgetError as error:
        assert text in str(error), (text, str(error))
    else:
        raise AssertionError('expected failure: ' + text)


def receipt(request_id, response_id=None, model=MODEL, known=True):
    return {'request_id': request_id, 'provider': 'openai', 'path': PATH,
            'request': {'model': model}, 'usage_known': known,
            'usage': {'input_tokens': 100, 'output_tokens': 10, 'cache_read_tokens': 0,
                      'cache_write_tokens': 0, 'cache_write_1h_tokens': 0, 'reasoning_tokens': 5},
            'terminal': {'complete': True, 'identity_matches': True,
                         'provider_response_id': response_id or request_id}}


def snapshot(gate):
    with gate.transaction() as database:
        return gate.totals(database)


def concurrent_worker(path, config, index, barrier, results):
    gate = budget.Gate(path, config, 'N3')
    barrier.wait()
    try:
        gate.reserve(str(index), 'openai', PATH, BODY)
        results.put(True)
    except budget.BudgetExhausted:
        results.put(False)


def concurrency(directory):
    path = os.path.join(directory, 'N3.sqlite3')
    config = policy()
    proposed = budget.quote(config, 'openai', PATH, BODY)['reserve_nanousd']
    config['cap_usd'] = str(Decimal(proposed * 2) / budget.NANOUSD_PER_USD)
    gate = budget.Gate(path, config, 'N3')
    context = multiprocessing.get_context('fork')
    barrier = context.Barrier(3)
    results = context.Queue()
    workers = [context.Process(target=concurrent_worker, args=(path, config, index, barrier, results)) for index in range(3)]
    for worker in workers:
        worker.start()
    for worker in workers:
        worker.join(timeout=30)
        assert worker.exitcode == 0
    assert sum(results.get(timeout=1) for _ in range(3)) == 2
    settled, reserved, count = snapshot(gate)
    assert settled == 0 and reserved == proposed * 2 and count == 2


def thread_concurrency(directory):
    config = policy()
    gate = budget.Gate(os.path.join(directory, 'threads.sqlite3'), config, 'N3')
    barrier = threading.Barrier(3)
    def worker(index):
        barrier.wait()
        gate.reserve(str(index), 'openai', PATH, BODY)
        gate.finish(str(index), receipt(str(index)))
    with concurrent.futures.ThreadPoolExecutor(3) as workers:
        list(workers.map(worker, range(3)))
    assert snapshot(gate) == (45000, 0, 3)


def lifecycle(directory):
    path = os.path.join(directory, 'life.sqlite3')
    gate = budget.Gate(path, policy(), 'first')
    proposed = gate.reserve('one', 'openai', PATH, BODY)
    assert proposed['input_tokens_max'] > len(BODY)
    expect_error(lambda: gate.reserve('one', 'openai', PATH, BODY), 'duplicate')
    restarted = budget.Gate(path, policy(), 'second')
    expect_error(lambda: restarted.reserve('two', 'openai', PATH, BODY), 'earlier')
    gate.finish('one', receipt('one'))
    gate.finish('one', receipt('one'))
    assert snapshot(gate) == (15000, 0, 1)
    restarted.reserve('two', 'openai', PATH, BODY)
    expect_error(lambda: restarted.finish('two', receipt('two', known=False)), 'unknown')
    assert snapshot(gate)[1] == proposed['reserve_nanousd']
    expect_error(lambda: restarted.reserve('three', 'openai', PATH, BODY), 'halted')
    expect_error(lambda: budget.Gate(path, policy('9'), 'third'), 'changed')


def unknown_cases(directory):
    for index, mutation in enumerate(('timeout', 'duplicate', 'overflow', 'negative', 'reasoning', 'identity', 'connect')):
        gate = budget.Gate(os.path.join(directory, mutation + '.sqlite3'), policy(), 'first')
        proposed = gate.reserve('one', 'openai', PATH, BODY)
        record = receipt('one')
        if mutation == 'timeout':
            gate.abandon('one')
        elif mutation == 'connect':
            record['not_dispatched'] = {'schema': 1, 'reason': 'connect_failed_before_request'}
            record['usage_known'] = False
            expect_error(lambda: gate.finish('one', record), 'reservation retained')
        elif mutation == 'duplicate':
            gate.finish('one', record)
            record['usage']['input_tokens'] += 1
            expect_error(lambda: gate.finish('one', record), 'conflicting')
            expect_error(lambda: gate.reserve('two', 'openai', PATH, BODY), 'halted')
            continue
        else:
            if mutation == 'overflow':
                record['usage']['output_tokens'] = 101
            elif mutation == 'negative':
                record['usage']['input_tokens'] = -1
            elif mutation == 'reasoning':
                record['usage']['reasoning_tokens'] = 11
            else:
                record['request']['model'] = 'gpt-6.1-terra'
            expect_error(lambda: gate.finish('one', record), 'reservation retained')
        assert snapshot(gate)[1] == proposed['reserve_nanousd']
        expect_error(lambda: gate.reserve('two', 'openai', PATH, BODY), 'halted')


def bounds(directory):
    config = policy()
    request = {'model': MODEL, 'input': '\U0001f600' * 100, 'max_output_tokens': 100}
    body = json.dumps(request, ensure_ascii=False).encode()
    result = budget.quote(config, 'openai', PATH, body)
    assert result['encoded_request_bytes'] > len(body)
    assert result['input_tokens_max'] >= len(body) + 1000
    for change, text in (({'tools': [{'type': 'web_search'}]}, 'server-side'),
                         ({'service_tier': 'priority'}, 'tier'),
                         ({'previous_response_id': 'old'}, 'hidden context'),
                         ({'input': [{'type': 'input_image'}]}, 'multimodal'),
                         ({'max_output_tokens': None}, 'positive'),
                         ({'model': 'unknown'}, 'pinned bounds')):
        changed = dict(request, **change)
        expect_error(lambda: budget.quote(config, 'openai', PATH, json.dumps(changed).encode()), text)
    expect_error(lambda: budget.quote(config, 'openai', '/evil', BODY), 'endpoint')
    expect_error(lambda: budget.quote(config, 'openai', PATH, b'{"model":"gpt-6-luna","model":"gpt-6-luna"}'), 'duplicate JSON')
    config['models'][MODEL]['provider'] = 'chatgpt'
    config['models'][MODEL]['output_bound'] = 'platform'
    expect_error(lambda: budget.validate_policy(config), 'no verified platform')


def cost_bounds():
    for model, prices in budget.rates().items():
        for cache in ('cache_read_tokens', 'cache_write_tokens', 'cache_write_1h_tokens'):
            usage = {'input_tokens': 10, 'output_tokens': 100, 'cache_read_tokens': 0,
                     'cache_write_tokens': 0, 'cache_write_1h_tokens': 0, 'reasoning_tokens': 100}
            usage[cache] = 1000
            if cache == 'cache_write_1h_tokens':
                usage['cache_write_tokens'] = 1000
            assert budget.usage_cost(model, usage) <= 1010 * max(prices.values()) + 100 * prices['output']


def no_network(directory):
    class FakeMeter(meter.Meter):
        command = 'POST'
        path = '/run/fake/openai/v1/responses'
        client_address = ('127.0.0.1', 123)
        clients = None
        headers = {}
        keys = {'openai': 'unused'}
        budget_gate = None
        incarnation = 'fake-meter'
        def __init__(self):
            self.runs = directory
            self.wfile = io.BytesIO()
        def send_error(self, code, message):
            self.error = (code, message)
        def send_response(self, code):
            self.status_code = code
        def send_header(self, name, value):
            pass
        def end_headers(self):
            pass
    class FakeResponse:
        status = 200
        def __init__(self, body):
            self.body = body
        def getheader(self, name):
            return 'application/json' if name == 'content-type' else None
        def getheaders(self):
            return []
        def read1(self, count):
            body, self.body = self.body, b''
            return body
    class FakeConnection:
        def connect(self):
            assert snapshot(handler.budget_gate)[1] > 0
        def request(self, method, path, body, headers):
            assert snapshot(handler.budget_gate)[1] > 0
            assert method == 'POST' and path == PATH and body == BODY
            self.dispatched = True
        def getresponse(self):
            return FakeResponse(json.dumps({'object': 'response', 'id': 'response-fake',
                'model': MODEL, 'status': 'completed', 'output': [],
                'usage': {'input_tokens': 100, 'output_tokens': 10,
                    'input_tokens_details': {'cached_tokens': 0},
                    'output_tokens_details': {'reasoning_tokens': 5}}}).encode())
        def close(self):
            pass
    os.mkdir(os.path.join(directory, 'fake'))
    handler = FakeMeter()
    original = meter.upstream
    def forbidden(origin):
        raise AssertionError('network attempted')
    meter.upstream = forbidden
    try:
        handler.budget_gate = budget.Gate(os.path.join(directory, 'denied.sqlite3'), policy(), handler.incarnation)
        for command, path in (('POST', '/responses/compact'), ('POST', '/responses/'),
                              ('POST', '/%72esponses'), ('POST', '/models'),
                              ('GET', '/unknown')):
            handler.command = command
            handler.path = '/run/fake/openai/v1' + path
            handler.forward(BODY)
            assert handler.error[0] == 403
        config = policy()
        proposed = budget.quote(config, 'openai', PATH, BODY)['reserve_nanousd']
        config['cap_usd'] = str(Decimal(proposed) / budget.NANOUSD_PER_USD)
        handler.budget_gate = budget.Gate(os.path.join(directory, 'forward.sqlite3'), config, handler.incarnation)
        handler.command = 'POST'
        handler.path = '/run/fake/openai/v1/responses?extra=true'
        handler.forward(BODY)
        assert handler.error[0] == 503
        handler.path = '/run/fake/openai/v1/responses'
        connection = FakeConnection()
        meter.upstream = lambda origin: connection
        handler.forward(BODY)
        assert connection.dispatched and handler.status_code == 200
        assert snapshot(handler.budget_gate) == (15000, 0, 1)
        meter.upstream = forbidden
        handler.forward(BODY)
        assert handler.error[0] == 503 and 'cap exhausted' in handler.error[1]
    finally:
        meter.upstream = original


def slack_reuse(directory):
    config = policy()
    proposed = budget.quote(config, 'openai', PATH, BODY)['reserve_nanousd']
    config['cap_usd'] = str(Decimal(proposed + 15000) / budget.NANOUSD_PER_USD)
    gate = budget.Gate(os.path.join(directory, 'slack.sqlite3'), config, 'first')
    gate.reserve('one', 'openai', PATH, BODY)
    gate.finish('one', receipt('one'))
    gate.reserve('two', 'openai', PATH, BODY)
    changed = receipt('one')
    changed['usage']['input_tokens'] += 1
    expect_error(lambda: gate.finish('one', changed), 'conflicting')
    assert snapshot(gate) == (15000, proposed, 2)
    expect_error(lambda: gate.reserve('three', 'openai', PATH, BODY), 'halted')


def platform_bounds(directory):
    context_path = os.path.join(directory, 'fake-context.json')
    output_path = os.path.join(directory, 'fake-context-definition.html')
    context = json.dumps([{'slug': MODEL, 'max_context_window': 1000, 'supported_in_api': True, 'input_modalities': ['text', 'image']}]).encode()
    output = b'The context window is the maximum number of tokens that can be used in a single request. This max tokens number includes input, output, and reasoning tokens.'
    for path, raw in ((context_path, context), (output_path, output)):
        with open(path, 'wb') as handle:
            handle.write(raw)
    config = policy()
    bound = config['models'][MODEL]
    config['models'] = {MODEL: bound}
    bound.update(provider='chatgpt', output_bound='platform', output_tokens_max=1000,
                 platform_contract='openai-total-context-including-reasoning',
                 context_evidence={'source': 'https://chatgpt.com/backend-api/codex/models?client_version=fake',
                     'path': context_path, 'sha256': hashlib.sha256(context).hexdigest()},
                 output_evidence={'source': 'https://developers.openai.com/api/docs/guides/conversation-state?fake',
                     'path': output_path, 'sha256': hashlib.sha256(output).hexdigest()})
    budget.validate_policy(config)
    body = json.dumps({'model': MODEL, 'input': 'hello'}).encode()
    unchanged = bytes(body)
    proposed = budget.quote(config, 'chatgpt', '/backend-api/codex/responses', body)
    assert body == unchanged
    assert proposed['output_tokens_max'] == 1000
    assert proposed['input_tokens_max'] >= 1000 + len(body)
    expect_error(lambda: budget.quote(config, 'chatgpt', '/backend-api/codex/responses', BODY), 'unsupported')
    image_path = os.path.join(directory, 'fake-image-price.html')
    image_contract = 'Vision models convert image inputs into billable input tokens. Apply the model’s input price to those tokens once'.encode()
    with open(image_path, 'wb') as handle:
        handle.write(image_contract)
    bound['allow_input_images'] = True
    bound['image_evidence'] = {'source': 'https://developers.openai.com/api/docs/guides/images-vision?fake',
                             'path': image_path, 'sha256': hashlib.sha256(image_contract).hexdigest()}
    budget.validate_policy(config)
    for image_url in ('data:image/png;base64,aGVsbG8=', 'https://example.com/image.png', 'data:image/png;base64,a'):
        image_body = json.dumps({'model': MODEL, 'input': [{'type': 'input_image', 'image_url': image_url}]}).encode()
        if image_url.endswith('aGVsbG8='):
            assert budget.quote(config, 'chatgpt', '/backend-api/codex/responses', image_body)['output_tokens_max'] == 1000
        else:
            expect_error(lambda: budget.quote(config, 'chatgpt', '/backend-api/codex/responses', image_body), 'input_image')
    bound['output_tokens_max'] = 100
    expect_error(lambda: budget.validate_policy(config), 'full provider maximum')
    bound['output_tokens_max'] = 1000
    with open(context_path, 'ab') as handle:
        handle.write(b' ')
    expect_error(lambda: budget.validate_policy(config), 'hash mismatch')


def main():
    with tempfile.TemporaryDirectory(prefix='budget-drill-') as temporary:
        for repetition in range(3):
            directory = os.path.join(temporary, str(repetition))
            os.mkdir(directory)
            concurrency(directory)
            thread_concurrency(directory)
            lifecycle(directory)
            slack_reuse(directory)
            unknown_cases(directory)
            bounds(directory)
            cost_bounds()
            platform_bounds(directory)
            no_network(directory)
    print('budget drill passed N3: process/thread concurrency, restart, duplicate, unknown/timeout, Unicode/JSON overhead, cost bounds, platform evidence, network refusal')


if __name__ == '__main__':
    main()
