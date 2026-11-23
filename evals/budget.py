import contextlib
import base64
import binascii
import hashlib
import html
import re
import json
import os
import sqlite3
from decimal import Decimal, InvalidOperation

import reporting_rules

POLICY_ENV = 'NANOEVAL_BUDGET_CONFIG'
CAP_USD_MAX = Decimal('10')
NANOUSD_PER_USD = 1000000000
TOKENS_PER_MILLION = 1000000
SCHEMA = 1
MODEL_PATHS = {
    'anthropic': ('/v1/messages',),
    'openai': ('/v1/responses', '/v1/chat/completions'),
    'chatgpt': ('/backend-api/codex/responses',),
}


class BudgetError(ValueError):
    pass


class BudgetExhausted(BudgetError):
    pass


def digest(value):
    raw = json.dumps(value, sort_keys=True, separators=(',', ':'), allow_nan=False).encode()
    return hashlib.sha256(raw).hexdigest()


def positive(value, name):
    if type(value) is not int or value <= 0:
        raise BudgetError(name + ' must be a positive integer')
    return value


def money(value):
    try:
        amount = Decimal(str(value))
        units = amount * NANOUSD_PER_USD
        if not amount.is_finite() or amount <= 0 or amount > CAP_USD_MAX or units != units.to_integral_value():
            raise BudgetError('cap must be positive exact nanoUSD, at most 10 USD')
        return int(units)
    except InvalidOperation as error:
        raise BudgetError('invalid budget cap') from error


def rates():
    result = {}
    for model, prices in reporting_rules.PRICES_PER_MTOK.items():
        converted = {key: Decimal(str(value)) * NANOUSD_PER_USD / TOKENS_PER_MILLION
                     for key, value in prices.items()}
        if any(not value.is_finite() or value < 0 or value != value.to_integral_value() for value in converted.values()):
            raise BudgetError('rate card is not exact nanoUSD per token')
        result[model] = {key: int(value) for key, value in converted.items()}
    return result


def evidence_bytes(evidence, name):
    if not isinstance(evidence, dict) or not isinstance(evidence.get('source'), str) or not evidence['source']:
        raise BudgetError(name + ' needs a source')
    checksum = evidence.get('sha256')
    if not isinstance(checksum, str) or len(checksum) != 64 or any(char not in '0123456789abcdef' for char in checksum):
        raise BudgetError(name + ' needs a pinned evidence SHA256')
    path = evidence.get('path')
    if not isinstance(path, str) or not os.path.isabs(path):
        raise BudgetError(name + ' needs an absolute local evidence path')
    with open(path, 'rb') as handle:
        raw = handle.read()
    if hashlib.sha256(raw).hexdigest() != checksum:
        raise BudgetError(name + ' hash mismatch')
    return raw


def platform_bound(model, bound, context, output):
    if bound['provider'] != 'chatgpt' or bound.get('platform_contract') != 'openai-total-context-including-reasoning':
        raise BudgetError('no verified platform billable-output ceiling; dispatch blocked')
    if not bound['context_evidence']['source'].startswith('https://chatgpt.com/backend-api/codex/models?'):
        raise BudgetError('platform bound needs the live route model metadata')
    if not bound['output_evidence']['source'].startswith('https://developers.openai.com/api/docs/guides/conversation-state'):
        raise BudgetError('platform bound needs the provider total-context definition')
    try:
        models = json.loads(context)
    except ValueError as error:
        raise BudgetError('invalid provider metadata') from error
    if isinstance(models, dict):
        models = models.get('models')
    if not isinstance(models, list):
        raise BudgetError('invalid provider model list')
    matching = [item for item in models if isinstance(item, dict) and item.get('slug') == model]
    if len(matching) != 1:
        raise BudgetError('provider model metadata is absent or ambiguous')
    if matching[0].get('supported_in_api') is not True:
        raise BudgetError('provider does not support this model for API requests')
    if bound.get('allow_input_images') is True:
        image_price_contract(bound, matching[0])
    ceiling = positive(matching[0].get('max_context_window'), 'provider max_context_window')
    if bound['context_tokens_max'] != ceiling or bound['output_tokens_max'] != ceiling:
        raise BudgetError('platform reservation must use the full provider maximum context')
    text = html.unescape(re.sub('<[^>]+>', ' ', output.decode('utf-8')))
    text = ' '.join(text.split())
    if 'context window is the maximum number of tokens that can be used in a single request.' not in text or 'includes input, output, and reasoning tokens.' not in text:
        raise BudgetError('provider documentation does not establish a total billable-context ceiling')


def validate_policy(policy):
    if not isinstance(policy, dict) or policy.get('schema') != SCHEMA:
        raise BudgetError('invalid budget policy schema')
    money(policy.get('cap_usd'))
    positive(policy.get('requests_max'), 'requests_max')
    models = policy.get('models')
    if not isinstance(models, dict) or not models:
        raise BudgetError('policy needs model bounds, including escalation models')
    for model, bound in models.items():
        if model not in rates() or not isinstance(bound, dict):
            raise BudgetError('model has no pinned list rate: ' + str(model))
        if bound.get('provider') not in MODEL_PATHS:
            raise BudgetError('invalid policy provider')
        for key in ('context_tokens_max', 'output_tokens_max', 'request_bytes_max'):
            positive(bound.get(key), key)
        if bound.get('output_bound') not in ('request', 'platform'):
            raise BudgetError('output_bound must be request or platform')
        context = evidence_bytes(bound.get('context_evidence'), 'context_evidence')
        output = evidence_bytes(bound.get('output_evidence'), 'output_evidence')
        if bound['output_bound'] == 'platform':
            platform_bound(model, bound, context, output)
        elif bound['provider'] == 'chatgpt':
            raise BudgetError('ChatGPT max_output_tokens unsupported; no verified platform output ceiling')
    return policy


def image_price_contract(bound, metadata):
    evidence = bound.get('image_evidence')
    raw = evidence_bytes(evidence, 'image_evidence')
    if evidence['source'].split('?')[0] != 'https://developers.openai.com/api/docs/guides/images-vision':
        raise BudgetError('input images need the official vision input price contract')
    text = ' '.join(html.unescape(re.sub('<[^>]+>', ' ', raw.decode('utf-8'))).split())
    if 'Vision models convert image inputs into billable input tokens.' not in text or 'Apply the model’s input price to those tokens once' not in text:
        raise BudgetError('image input price contract does not establish the pinned token rate')
    if 'image' not in (metadata.get('input_modalities') or []):
        raise BudgetError('provider model does not support image inputs')


def local_image(value):
    if set(value) - {'type', 'image_url', 'detail'} or value.get('detail', 'auto') not in ('auto', 'low', 'high', 'original'):
        raise BudgetError('unsupported local input_image shape')
    url = value.get('image_url')
    if not isinstance(url, str) or not re.fullmatch(r'data:image/(png|jpeg|webp|gif);base64,[A-Za-z0-9+/]+={0,2}', url):
        raise BudgetError('input_image must contain local base64 data, not a remote URL')
    try:
        if not base64.b64decode(url.split(',', 1)[1], validate=True):
            raise BudgetError('empty input_image data')
    except binascii.Error as error:
        raise BudgetError('invalid input_image base64') from error


def validate_request(request, provider, allow_input_images=False):
    if request.get('speed') not in (None, 'standard'):
        raise BudgetError('unpriced inference speed')
    if request.get('service_tier') not in (None, 'default'):
        raise BudgetError('unpriced service tier')
    if type(request.get('n', 1)) is not int or request.get('n', 1) != 1:
        raise BudgetError('multiple output choices are not bounded')
    tools = request.get('tools') or []
    if not isinstance(tools, list):
        raise BudgetError('invalid tools')
    for tool in tools:
        if not isinstance(tool, dict):
            raise BudgetError('invalid tool')
        kind = tool.get('type')
        if provider == 'anthropic' and kind is None and isinstance(tool.get('name'), str):
            continue
        if kind not in ('function', 'custom'):
            raise BudgetError('unpriced server-side tool')
    if any(key in request for key in ('previous_response_id', 'conversation', 'file_ids', 'attachments')):
        raise BudgetError('hidden context or remote input is not locally bounded')
    pending = [request]
    while pending:
        value = pending.pop()
        if isinstance(value, dict):
            kind = value.get('type')
            if kind == 'input_image' and allow_input_images:
                local_image(value)
                continue
            if isinstance(kind, str) and any(part in kind for part in ('image', 'audio', 'video', 'file', 'computer')):
                raise BudgetError('multimodal or remote input is not locally bounded')
            if any(key in value for key in ('image_url', 'file_id', 'file_url', 'audio', 'video')):
                raise BudgetError('remote input is not locally bounded')
            pending.extend(value.values())
        elif isinstance(value, list):
            pending.extend(value)


def configuration(path):
    with open(path) as handle:
        policy = validate_policy(json.load(handle))
    ledger_path = policy.get('ledger_path')
    if not isinstance(ledger_path, str) or not os.path.isabs(ledger_path):
        raise BudgetError('configuration needs one absolute shared ledger_path')
    expected = {'rates_nanousd': rates(), 'reporting_rules': reporting_rules.pin()}
    if '_pins' in policy and policy['_pins'] != expected:
        raise BudgetError('budget configuration rate/implementation pins changed')
    policy['_pins'] = expected
    return policy


def unique_object(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise BudgetError('duplicate JSON key')
        value[key] = item
    return value


def quote(policy, provider, path, body):
    if path.split('?')[0] not in MODEL_PATHS.get(provider, ()):
        raise BudgetError('unrecognized inference endpoint')
    if len(body) > max(bound['request_bytes_max'] for bound in policy['models'].values()):
        raise BudgetExhausted('request_bytes_max exhausted')
    try:
        request = json.loads(body, object_pairs_hook=unique_object)
    except BudgetError:
        raise
    except (ValueError, UnicodeError, RecursionError) as error:
        raise BudgetError('invalid inference JSON') from error
    if not isinstance(request, dict):
        raise BudgetError('inference request must be an object')
    model = request.get('model')
    bound = policy['models'].get(model) if isinstance(model, str) else None
    if bound is None or bound['provider'] != provider:
        raise BudgetError('model/provider lacks pinned bounds: ' + str(model))
    allow_images = bound['output_bound'] == 'platform' and bound.get('allow_input_images') is True
    validate_request(request, provider, allow_images)
    if model == 'gpt-5.5' and bound['context_tokens_max'] >= 272000:
        raise BudgetError('pinned gpt-5.5 prices only cover prompts below 272000 tokens')
    if len(body) > bound['request_bytes_max']:
        raise BudgetExhausted('request_bytes_max exhausted')
    try:
        encoded_bytes = len(json.dumps(request, ensure_ascii=True, allow_nan=False).encode('ascii'))
    except (ValueError, RecursionError) as error:
        raise BudgetError('request cannot be conservatively encoded') from error
    input_max = bound['context_tokens_max'] + max(len(body), encoded_bytes)
    if model == 'gpt-5.5' and input_max >= 272000:
        raise BudgetError('conservative input bound exceeds pinned standard-context pricing')
    output_max = bound['output_tokens_max']
    if provider == 'chatgpt' and any(key in request for key in ('max_output_tokens', 'max_tokens', 'max_completion_tokens')):
        raise BudgetError('ChatGPT output-limit parameters are unsupported; body was not modified')
    if bound['output_bound'] == 'request':
        field = 'max_tokens' if provider == 'anthropic' else 'max_completion_tokens' if path.split('?')[0].endswith('/chat/completions') else 'max_output_tokens'
        wire_max = positive(request.get(field), field)
        if wire_max > output_max:
            raise BudgetError('wire output limit exceeds experiment output bound')
        output_max = wire_max
    if provider == 'anthropic':
        input_rate = max(rates()[model][key] for key in ('input', 'cache_read', 'cache_write_5m', 'cache_write_1h'))
    else:
        input_rate = max(rates()[model]['input'], rates()[model]['cache_read'])
    proposed = input_max * input_rate + output_max * rates()[model]['output']
    return {'model': model, 'provider': provider, 'path': path.split('?')[0],
            'body_sha256': hashlib.sha256(body).hexdigest(), 'input_tokens_max': input_max,
            'output_tokens_max': output_max, 'reserve_nanousd': proposed,
            'encoded_request_bytes': encoded_bytes, 'raw_request_bytes': len(body)}


def usage_cost(model, usage):
    if not isinstance(usage, dict):
        raise BudgetError('missing usage')
    keys = ('input_tokens', 'output_tokens', 'cache_read_tokens', 'cache_write_tokens', 'cache_write_1h_tokens', 'reasoning_tokens')
    if any(type(usage.get(key)) is not int or usage[key] < 0 for key in keys):
        raise BudgetError('invalid usage counters')
    if usage['cache_write_1h_tokens'] > usage['cache_write_tokens'] or usage['reasoning_tokens'] > usage['output_tokens']:
        raise BudgetError('inconsistent usage counters')
    prices = rates()[model]
    return (usage['input_tokens'] * prices['input'] + usage['output_tokens'] * prices['output']
            + usage['cache_read_tokens'] * prices['cache_read']
            + (usage['cache_write_tokens'] - usage['cache_write_1h_tokens']) * prices['cache_write_5m']
            + usage['cache_write_1h_tokens'] * prices['cache_write_1h'])


class Gate:
    def __init__(self, path, policy, incarnation):
        self.path = os.path.abspath(path)
        if policy.get('ledger_path') is not None and self.path != policy['ledger_path']:
            raise BudgetError('use the policy shared ledger_path, not a new per-campaign ledger')
        self.policy = validate_policy(policy)
        self.incarnation = incarnation
        self.cap = money(policy['cap_usd'])
        self.pin = digest({'policy': policy, 'reporting_rules': reporting_rules.pin(), 'rates_nanousd': rates()})
        with self.transaction() as database:
            database.execute('CREATE TABLE IF NOT EXISTS settings (id INTEGER PRIMARY KEY CHECK(id=1), pin TEXT NOT NULL, cap INTEGER NOT NULL, halt TEXT)')
            database.execute('CREATE TABLE IF NOT EXISTS requests (id TEXT PRIMARY KEY, incarnation TEXT NOT NULL, quote TEXT NOT NULL, reserve INTEGER NOT NULL, state TEXT NOT NULL, cost INTEGER, receipt TEXT, response_id TEXT UNIQUE)')
            row = database.execute('SELECT pin, cap FROM settings WHERE id=1').fetchone()
            if row is None:
                database.execute('INSERT INTO settings VALUES (1,?,?,NULL)', (self.pin, self.cap))
            elif row != (self.pin, self.cap):
                raise BudgetError('budget policy/rates changed; do not reset or replace this ledger')
        descriptor = os.open(os.path.dirname(self.path), os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)

    @contextlib.contextmanager
    def transaction(self):
        database = sqlite3.connect(self.path, timeout=30, isolation_level=None)
        try:
            database.execute('PRAGMA synchronous=FULL')
            database.execute('PRAGMA journal_mode=DELETE')
            database.execute('BEGIN IMMEDIATE')
            yield database
            database.commit()
        except BaseException:
            database.rollback()
            raise
        finally:
            database.close()

    def totals(self, database):
        settled, reserved, count = database.execute("SELECT COALESCE(SUM(CASE WHEN state='settled' THEN cost ELSE 0 END),0), COALESCE(SUM(CASE WHEN state!='settled' THEN reserve ELSE 0 END),0), COUNT(*) FROM requests").fetchone()
        if settled < 0 or reserved < 0 or settled + reserved > self.cap:
            raise BudgetError('budget ledger invariant violated')
        return settled, reserved, count

    def status(self):
        with self.transaction() as database:
            settled, reserved, count = self.totals(database)
            halt = database.execute('SELECT halt FROM settings WHERE id=1').fetchone()[0]
            stale = database.execute("SELECT 1 FROM requests WHERE state='unknown' OR (state='active' AND incarnation!=?) LIMIT 1", (self.incarnation,)).fetchone()
            return {'cap_nanousd': self.cap, 'settled_nanousd': settled,
                    'reserved_nanousd': reserved, 'requests': count,
                    'policy_sha256': self.pin,
                    'halt': halt or ('unresolved request from an earlier meter incarnation' if stale else None)}

    def reserve(self, request_id, provider, path, body):
        proposed = quote(self.policy, provider, path, body)
        exhausted = None
        with self.transaction() as database:
            halt = database.execute('SELECT halt FROM settings WHERE id=1').fetchone()[0]
            if halt:
                raise BudgetError('budget halted: ' + halt)
            if database.execute("SELECT 1 FROM requests WHERE state='unknown' OR (state='active' AND incarnation!=?) LIMIT 1", (self.incarnation,)).fetchone():
                raise BudgetError('unresolved request from an earlier meter incarnation')
            if database.execute('SELECT 1 FROM requests WHERE id=?', (request_id,)).fetchone():
                raise BudgetError('duplicate request admission')
            settled, reserved, count = self.totals(database)
            if count >= self.policy['requests_max']:
                exhausted = 'requests_max exhausted'
            elif settled + reserved + proposed['reserve_nanousd'] > self.cap:
                exhausted = 'list-price-equivalent cap exhausted'
            if exhausted:
                database.execute('UPDATE settings SET halt=? WHERE id=1', (exhausted,))
            else:
                database.execute('INSERT INTO requests VALUES (?,?,?,?,\'active\',NULL,NULL,NULL)',
                    (request_id, self.incarnation, json.dumps(proposed, sort_keys=True), proposed['reserve_nanousd']))
                self.totals(database)
        if exhausted:
            raise BudgetExhausted(exhausted)
        return proposed

    def finish(self, request_id, record):
        receipt = digest(record)
        with self.transaction() as database:
            row = database.execute('SELECT quote,reserve,state,receipt FROM requests WHERE id=?', (request_id,)).fetchone()
            if row is None:
                raise BudgetError('terminal request has no budget reservation')
            proposed, reserve, state, previous = json.loads(row[0]), row[1], row[2], row[3]
            if previous == receipt:
                return
            reason, cost, response_id = self.terminal(database, request_id, proposed, reserve, record)
            if state != 'active':
                reason = 'conflicting duplicate terminal receipt'
            if reason:
                if state != 'settled':
                    database.execute("UPDATE requests SET state='unknown', cost=NULL, receipt=? WHERE id=?", (receipt, request_id))
                database.execute('UPDATE settings SET halt=? WHERE id=1', (reason,))
            else:
                database.execute("UPDATE requests SET state='settled', cost=?,receipt=?,response_id=? WHERE id=?", (cost, receipt, response_id, request_id))
            self.totals(database)
        if reason:
            raise BudgetError(reason + '; reservation retained, dispatch halted')

    def abandon(self, request_id):
        with self.transaction() as database:
            row = database.execute('SELECT state FROM requests WHERE id=?', (request_id,)).fetchone()
            if row is None:
                raise BudgetError('abandoned request has no reservation')
            if row[0] == 'active':
                database.execute("UPDATE requests SET state='unknown' WHERE id=?", (request_id,))
                database.execute("UPDATE settings SET halt='ambiguous dispatch or interrupted persistence' WHERE id=1")
            self.totals(database)

    def terminal(self, database, request_id, proposed, reserve, record):
        matches = record.get('request_id') == request_id and all(record.get(key) == proposed[key] for key in ('provider', 'path'))
        matches = matches and (record.get('request') or {}).get('model') == proposed['model']
        if not matches:
            return 'terminal does not match reservation', None, None
        terminal = record.get('terminal') or {}
        response_id = terminal.get('provider_response_id')
        if record.get('usage_known') is not True or terminal.get('complete') is not True or terminal.get('identity_matches') is not True or not isinstance(response_id, str) or not response_id:
            return 'unknown usage or ambiguous dispatch', None, None
        if database.execute('SELECT 1 FROM requests WHERE response_id=? AND id!=?', (response_id, request_id)).fetchone():
            return 'duplicate provider response identity', None, None
        try:
            cost = usage_cost(proposed['model'], record.get('usage'))
        except BudgetError as error:
            return str(error), None, None
        usage = record['usage']
        input_tokens = usage['input_tokens'] + usage['cache_read_tokens'] + usage['cache_write_tokens']
        if input_tokens > proposed['input_tokens_max'] or usage['output_tokens'] > proposed['output_tokens_max'] or cost > reserve:
            return 'observed usage exceeds pinned reservation bound', None, None
        return None, cost, response_id


def from_environment(runs, incarnation):
    policy_path = os.environ.get(POLICY_ENV)
    if not policy_path:
        return None
    policy = configuration(policy_path)
    return Gate(policy['ledger_path'], policy, incarnation)
