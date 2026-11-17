import json
import re
import sys

ROOT = sys.argv[1] if len(sys.argv) > 1 else "."
PROMPTS = ROOT + "/scripts/fixtures/prompts/"
CHUNK = 24000


def L(text):
    assert "\r" not in text and "\t" not in text, repr(text[:40])
    return '"' + text.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'


def J(value):
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def Jstr(text):
    return J(text)[1:-1]


def read(name):
    with open(name if "/" in name else PROMPTS + name, encoding="utf-8") as file:
        return file.read()


BS = "\\"
AWK = (
    "BEGIN { n = 0 } { for (i = 1; i <= NF; i++) { b[n] = $i + 0; n++ } } "
    "END { while (n > 0 && (b[n-1] == 32 || b[n-1] == 9 || b[n-1] == 10 || b[n-1] == 13 || b[n-1] == 12 || b[n-1] == 11)) n--; "
    "i = 0; while (i < n) { c = b[i]; "
    'if (c == 34) { printf "%s", "BBBQ"; i++ } '
    'else if (c == 92) { printf "%s", "BBBB"; i++ } '
    'else if (c == 10) { printf "%s", "BBn"; i++ } '
    'else if (c == 9) { printf "%s", "BBt"; i++ } '
    'else if (c == 13) { printf "%s", "BBr"; i++ } '
    'else if (c < 32 || c == 127) { printf "BBu%04x", c; i++ } '
    'else if (c < 128) { printf "%c", c; i++ } '
    "else if (c >= 240 && c < 248 && i + 3 < n) { p = (c - 240) * 262144 + (b[i+1] - 128) * 4096 + (b[i+2] - 128) * 64 + (b[i+3] - 128) - 65536; "
    'printf "BBu%04xBBu%04x", 55296 + int(p / 1024), 56320 + p % 1024; i += 4 } '
    'else if (c >= 224 && c < 240 && i + 2 < n) { printf "BBu%04x", (c - 224) * 4096 + (b[i+1] - 128) * 64 + (b[i+2] - 128); i += 3 } '
    'else if (c >= 192 && c < 224 && i + 1 < n) { printf "BBu%04x", (c - 192) * 64 + (b[i+1] - 128); i += 2 } '
    'else { printf "%s", "BBufffd"; i++ } } }'
).replace("BBBQ", BS * 2 + BS + '"').replace("BBBB", BS * 4).replace("BB", BS * 2)


def probe_script(codex):
    lines = [
        'cd -- "$c" || exit 1',
        "e() { LC_ALL=C od -An -tu1 -v | LC_ALL=C awk '" + AWK + "'; }",
        "k=" + str(CHUNK),
        'if [ -z "$s" ]; then',
        "printf '@@hl-cwd@@'; printf '%s' \"$c\" | e",
        "printf '@@hl-git@@'; d=$c; g=false; while :; do if [ -e \"$d/.git\" ]; then g=true; break; fi; if [ \"$d\" = / ] || [ -z \"$d\" ]; then break; fi; d=$(dirname -- \"$d\"); done; printf '%s' \"$g\"",
        "printf '@@hl-platform@@'; uname -s 2>/dev/null | tr 'A-Z' 'a-z' | e",
        "printf '@@hl-shell@@'; if [ -n \"${SHELL:-}\" ]; then printf '%s' \"${SHELL##*/}\" | e; fi",
        "printf '@@hl-os@@'; uname -sr 2>/dev/null | e",
        "printf '@@hl-date@@'; date +%Y-%m-%d 2>/dev/null | e",
    ]
    if codex:
        lines += [
            "printf '@@hl-zone@@'; date +%Z 2>/dev/null | e",
            "printf '@@hl-key@@'; if [ -r /proc/sys/kernel/random/uuid ]; then tr -d '" + BS + "n' < /proc/sys/kernel/random/uuid; else od -An -tx1 -N16 /dev/urandom | tr -d ' " + BS + "n' | sed 's/^" + BS + "(........" + BS + ")" + BS + "(...." + BS + ")." + BS + "(..." + BS + ")." + BS + "(..." + BS + ")" + BS + "(............" + BS + ")$/" + BS + "1-" + BS + "2-4" + BS + "3-a" + BS + "4-" + BS + "5/'; fi",
            "printf '@@hl-base@@'; u=${OPENAI_BASE_URL:-}; n=$u; while :; do case $n in */) n=${n%/};; *) break;; esac; done; while :; do case $n in */v1) n=${n%/v1};; *) break;; esac; done; r=${u#\"$n\"}; if [ -z \"$u\" ]; then printf none; else case $r in '') printf plain;; /) printf slash;; /v1) printf v1;; /v1/) printf v1slash;; *) printf 'other@@hl-normal@@%s' \"$n\";; esac; fi",
        ]
    lines += CODEX_LOGIN_PROBE if codex else CLAUDE_LOGIN_PROBE
    lines += [
        "fi",
        "printf '@@hl-agents@@'",
        "if [ -f AGENTS.md ] && [ -r AGENTS.md ]; then printf 'present@@hl-text@@'; e < AGENTS.md | tail -c +$((${#s} * k + 1)) | head -c \"$k\"; t=$(e < AGENTS.md | wc -c); if [ \"$t\" -gt $(((${#s} + 1) * k)) ]; then printf '@@hl-more@@'; fi; fi",
        "printf '@@hl-end@@'",
    ]
    return "c='", "'; s='", "'\n" + "\n".join(lines)

Q = chr(34)
NL = chr(10)

CODEX_TOKEN_URL = "https://auth.openai.com/oauth/token"
CODEX_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"
CHATGPT_RESPONSES_URL = "https://chatgpt.com/backend-api/codex/responses"
CLAUDE_TOKEN_URL = "https://console.anthropic.com/v1/oauth/token"
CLAUDE_CLIENT_ID = "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
CLAUDE_MESSAGES_URL = "${ANTHROPIC_BASE_URL:-https://api.anthropic.com}/v1/messages"
CLAUDE_OAUTH_BETA = "oauth-2025-04-20"
CLAUDE_KEYCHAIN = "Claude Code-credentials"
REFRESH_CURL_SECONDS = 20


def yes_no(name, test):
    return "printf '@@hl-" + name + "@@'; if " + test + "; then printf yes; else printf no; fi"


CODEX_LOGIN_PROBE = [
    r'''printf '@@hl-login@@'; h=${CODEX_HOME:-$HOME/.codex}/auth.json; if [ -r "$h" ] && grep -q '"auth_mode": *"chatgpt"' "$h" && grep -q '"account_id": *"[^"]' "$h"; then printf chatgpt; printf '@@hl-loginfile@@'; printf '%s' "$h" | e; printf '@@hl-stale@@'; x=$(sed -n 's/.*"access_token": *"[^".]*\.\([^".]*\)\..*/\1/p' "$h" | head -n 1 | tr '_-' '/+'); while [ $((${#x} % 4)) -ne 0 ]; do x="$x="; done; t=$(printf '%s' "$x" | base64 -d 2>/dev/null | tr -d ' \n' | sed -n 's/.*"exp":\([0-9]*\).*/\1/p'); if [ -n "$t" ] && [ "$t" -le "$(date +%s)" ]; then printf yes; else printf no; fi; else printf none; fi''',
    yes_no("haskey", '[ -n "${OPENAI_API_KEY:-}" ]'),
]

CLAUDE_OAUTH_SED = r'''sed -n 's/.*"claudeAiOauth": *{\([^}]*\)}.*/\1/p' '''

CLAUDE_LOGIN_PROBE = [
    r'''printf '@@hl-login@@'; h=${CLAUDE_CONFIG_DIR:-$HOME/.claude}/.credentials.json; m=file; o=$(''' + CLAUDE_OAUTH_SED + r'''"$h" 2>/dev/null | head -n 1); if ! printf '%s' "$o" | grep -q '"accessToken": *"[^"]'; then if [ "$(uname -s)" = Darwin ] && k=$(security find-generic-password -s ''' + "'" + CLAUDE_KEYCHAIN + "'" + r''' -w 2>/dev/null) && [ -n "$k" ]; then h=$HOME/.hl/claude-credentials.json; m=keychain; mkdir -p "$HOME/.hl" && (umask 077 && printf '%s' "$k" > "$h.new" && mv -f "$h.new" "$h"); o=$(''' + CLAUDE_OAUTH_SED + r'''"$h" | head -n 1); fi; fi; if printf '%s' "$o" | grep -q '"accessToken": *"[^"]'; then printf '%s' "$m"; printf '@@hl-loginfile@@'; printf '%s' "$h" | e; printf '@@hl-stale@@'; x=$(printf '%s' "$o" | sed -n 's/.*"expiresAt": *\([0-9]*\).*/\1/p'); if [ -n "$x" ] && [ "$x" -le "$(date +%s)000" ]; then printf yes; else printf no; fi; else printf none; fi''',
    yes_no("haskey", '[ -n "${ANTHROPIC_API_KEY:-}" ]'),
    yes_no("hasbase", '[ -n "${ANTHROPIC_BASE_URL:-}" ]'),
    yes_no("hasoauth", '[ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]'),
]

LOGIN_FACTS = {
    True: ["login", "loginfile", "stale", "haskey"],
    False: ["login", "loginfile", "stale", "haskey", "hasbase", "hasoauth"],
}

REWRITE_AWK = r'''{ s = $0; h = ""; if (!on) { p = index(s, "\"" ENVIRON["HL_SCOPE"] "\""); if (p == 0) { print s; next } on = 1; h = substr(s, 1, p - 1); s = substr(s, p) } n = split(ENVIRON["HL_TEXTS"], t, " "); for (j = 1; j <= n; j++) if (!d[t[j]] && match(s, "\"" t[j] "\": *\"[^\"]*\"")) { s = substr(s, 1, RSTART - 1) "\"" t[j] "\": \"" ENVIRON["HL_" t[j]] "\"" substr(s, RSTART + RLENGTH); d[t[j]] = 1 } n = split(ENVIRON["HL_NUMBERS"], t, " "); for (j = 1; j <= n; j++) if (!d[t[j]] && match(s, "\"" t[j] "\": *[0-9]+")) { s = substr(s, 1, RSTART - 1) "\"" t[j] "\": " ENVIRON["HL_" t[j]] substr(s, RSTART + RLENGTH); d[t[j]] = 1 } print h s } END { n = split(ENVIRON["HL_TEXTS"] " " ENVIRON["HL_NUMBERS"], t, " "); for (j = 1; j <= n; j++) if (!d[t[j]]) exit 1 }'''

HELD_AWK = r'''index($0, ENVIRON["HL_HELD"]) { f = 1 } END { exit !f }'''

REFRESH_COMMON = [
    "umask 077",
    r'''v() { sed -n "s/.*\"$1\": *\"\([^\"]*\)\".*/\1/p" | head -n 1; }''',
    r'''say() { printf '@@hl-refresh@@%s@@hl-end@@' "$1"; exit 0; }''',
    r'''safe() { case $1 in '' | *[!A-Za-z0-9._-]*) say 'the token server answered an unexpected token';; esac; }''',
    r'''post() { curl -sS --max-time ''' + str(REFRESH_CURL_SECONDS) + r''' -X POST -H 'content-type: application/json' --data-binary @- -w '\n@@hl-status@@%{http_code}' "$1" 2>/dev/null; }''',
    r'''why() { e=$(printf '%s' "$b" | v error); [ -n "$e" ] || e=$(printf '%s' "$b" | v code); [ -n "$e" ] || e=$(printf '%s' "$b" | v type); printf 'HTTP %s %s' "$c" "$(printf '%s' "$e" | tr -cd 'A-Za-z0-9_ .-')"; }''',
    "w() { t=$(mktemp \"$f.XXXXXX\") || say 'no temporary file beside the login file'; if awk '" + REWRITE_AWK + "' \"$f\" > \"$t\" && HL_HELD=$1 awk '" + HELD_AWK + "' \"$t\"; then mv -f \"$t\" \"$f\"; else rm -f \"$t\"; say 'the login file did not take the new token'; fi; }",
]

CODEX_REFRESH = REFRESH_COMMON + [
    r'''r=$(v refresh_token < "$f"); [ -n "$r" ] || say 'the login has no refresh token'; safe "$r"''',
    r'''b=$(printf '{"client_id":"''' + CODEX_CLIENT_ID + r'''","grant_type":"refresh_token","refresh_token":"%s","scope":"openid profile email"}' "$r" | post "${CODEX_REFRESH_TOKEN_URL_OVERRIDE:-''' + CODEX_TOKEN_URL + r'''}")''',
    r'''c=${b##*@@hl-status@@}; b=${b%@@hl-status@@*}''',
    r'''a=$(printf '%s' "$b" | v access_token); i=$(printf '%s' "$b" | v id_token); n=$(printf '%s' "$b" | v refresh_token)''',
    r'''if [ "$c" != 200 ] || [ -z "$a" ]; then [ "$(v refresh_token < "$f")" = "$r" ] || say ok; say "$(why)"; fi''',
    r'''[ -n "$i" ] || i=$(v id_token < "$f"); [ -n "$n" ] || n=$r; safe "$a"; safe "$i"; safe "$n"''',
    r'''HL_SCOPE=tokens HL_TEXTS='access_token id_token refresh_token' HL_NUMBERS= HL_access_token=$a HL_id_token=$i HL_refresh_token=$n w "$a"''',
    r'''HL_SCOPE=last_refresh HL_TEXTS=last_refresh HL_NUMBERS= HL_last_refresh=$(date -u +%Y-%m-%dT%H:%M:%S.000000Z) w "$a"''',
    "say ok",
]

CLAUDE_REFRESH = REFRESH_COMMON + [
    r'''o() { ''' + CLAUDE_OAUTH_SED + r'''"$f" | head -n 1; }''',
    r'''if [ "$m" = keychain ]; then k=$(security find-generic-password -s ''' + "'" + CLAUDE_KEYCHAIN + "'" + r''' -w 2>/dev/null) && [ -n "$k" ] && printf '%s' "$k" > "$f.new" && mv -f "$f.new" "$f" || say 'the keychain holds no Claude login'; x=$(o | sed -n 's/.*"expiresAt": *\([0-9]*\).*/\1/p'); if [ -n "$x" ] && [ "$x" -gt "$(date +%s)000" ]; then say ok; fi; say 'the keychain login has expired; run claude once to renew it'; fi''',
    r'''r=$(o | v refreshToken); [ -n "$r" ] || say 'the login has no refresh token'; safe "$r"''',
    r'''b=$(printf '{"grant_type":"refresh_token","refresh_token":"%s","client_id":"''' + CLAUDE_CLIENT_ID + r'''"}' "$r" | post "${HL_CLAUDE_TOKEN_URL:-''' + CLAUDE_TOKEN_URL + r'''}")''',
    r'''c=${b##*@@hl-status@@}; b=${b%@@hl-status@@*}''',
    r'''a=$(printf '%s' "$b" | v access_token); n=$(printf '%s' "$b" | v refresh_token); x=$(printf '%s' "$b" | sed -n 's/.*"expires_in": *\([0-9]*\).*/\1/p' | head -n 1)''',
    r'''if [ "$c" != 200 ] || [ -z "$a" ] || [ -z "$x" ]; then [ "$(o | v refreshToken)" = "$r" ] || say ok; say "$(why)"; fi''',
    r'''[ -n "$n" ] || n=$r; safe "$a"; safe "$n"''',
    r'''HL_SCOPE=claudeAiOauth HL_TEXTS='accessToken refreshToken' HL_NUMBERS=expiresAt HL_accessToken=$a HL_refreshToken=$n HL_expiresAt=$(( ($(date +%s) + x) * 1000 )) w "$a"''',
    "say ok",
]

LOGIN_WORDS = {
    True: {
        "who": "ChatGPT",
        "none": "no model login: sign in with codex login for the ChatGPT plan, or set OPENAI_API_KEY",
        "again": "; sign in again with codex login",
    },
    False: {
        "who": "Claude",
        "none": "no model login: sign in with claude for the Claude subscription, or set CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY",
        "again": "; sign in again with claude",
    },
}


def login_fns(call_of, codex, otherwise):
    words = LOGIN_WORDS[codex]
    script = NL.join(CODEX_REFRESH if codex else CLAUDE_REFRESH)
    command = f"{L('f=' + chr(39))} ++ env_loginfile ++ {L(chr(39) + '; m=' + chr(39))} ++ env_login ++ {L(chr(39) + NL + script)}"
    out = ["fn choose_login() {", '  login_route = "key"']
    if codex:
        out += [
            '  if env_base != "" and env_base != "none" {',
            '    login_route = "base"',
            '  } else if env_login == "chatgpt" {',
            '    login_route = "subscription"',
        ]
    else:
        out += [
            '  if env_hasbase == "yes" and env_haskey == "yes" {',
            '    login_route = "base"',
            '  } else if env_login == "file" or env_login == "keychain" {',
            '    login_route = "subscription"',
        ]
    out += [
        f"    if contains(env_loginfile, {L(chr(39))}) or contains(env_loginfile, {L('}')}) or contains(env_loginfile, {L('#')}) or contains(env_loginfile, {L(BS)}) {{",
        f"      return failed({L('the ' + words['who'] + ' login file path holds a quote, a brace, a hash or a backslash')})",
        "    }",
        '    if env_stale == "yes" {',
        "      refresh_login()",
        "    }",
    ]
    if not codex:
        out += ['  } else if env_hasoauth == "yes" {', '    login_route = "token"']
    out += [
        '  } else if env_haskey == "no" {',
        f"    return failed({L(words['none'])})",
        "  }",
        "}",
        "",
        "fn refresh_login() {",
        "  login_refreshed = true",
        f"  match {call_of(command)} {{",
        "    ok(out) => { login_refresh_done(out) }",
        f"    failed(error) => {{ return failed({L('the ' + words['who'] + ' login refresh failed: ')} ++ error) }}",
        "    unknown => {}",
        "  }",
        "}",
        "",
        "fn login_refresh_done(out: text) {",
        '  env_stale = "no"',
        f"  if not contains(out, {L('@@hl-refresh@@ok@@hl-end@@')}) {{",
        f"    return failed({L('the ' + words['who'] + ' login refresh failed: ')} ++ before(after(out, {L('@@hl-refresh@@')}), {L('@@hl-end@@')}) ++ {L(words['again'])})",
        "  }",
        "}",
        "",
        "fn login_rejected(error: text) {",
        '  if login_route == "subscription" and not login_refreshed and contains(error, "HTTP 401") {',
        "    refresh_login()",
        "  } else {",
        f"    {otherwise}",
        "  }",
        "}",
        "",
    ]
    return out


def probe_fns(call_of, recoverable, codex):
    first, middle, script = probe_script(codex)
    command = f"{L(first)} ++ cwd ++ {L(middle)} ++ agents_skip ++ {L(script)}"
    out = [
        "fn probe(cwd: text) {",
        "  agents_more = true",
        "  agents_present = false",
        '  agents_text = ""',
        '  agents_skip = ""',
        "  loop chunk below agents_chunks_max {",
        "    if agents_more {",
        "      agents_more = false",
        f"      match {call_of(command)} {{",
        "        ok(out) => { probed_output(out, chunk) }",
        "        failed(_) => { probe_lost() }",
    ]
    if recoverable:
        out.append("        unknown => { probe_lost() }")
    out += [
        "      }",
        "    }",
        "  }",
        "  if agents_more {",
        '    return failed("AGENTS.md is longer than the environment probe reads")',
        "  }",
        "}",
        "",
        "fn probe_lost() {",
        "  agents_more = false",
        "  agents_present = false",
        '  agents_text = ""',
        "}",
        "",
        "fn probed_output(out: text, chunk: u64) {",
        '  if not contains(out, "@@hl-end@@") {',
        "    probe_lost()",
        "  } else {",
        "    probed_fields(out, chunk)",
        "  }",
        "}",
        "",
        "fn probed_fields(out: text, chunk: u64) {",
        "  if chunk == 0 {",
        "    probed = true",
    ]
    names = ["cwd", "git", "platform", "shell", "os", "date"] + (["zone", "key", "base", "normal"] if codex else []) + LOGIN_FACTS[codex]
    for name in names:
        field = {"key": "cache_key"}.get(name, "env_" + name)
        out.append(f'    {field} = before(after(out, "@@hl-{name}@@"), "@@hl-")')
    out += [
        "  }",
        '  let agents = before(after(out, "@@hl-agents@@"), "@@hl-end@@")',
        '  if contains(agents, "present@@hl-text@@") {',
        "    agents_present = true",
        '    agents_text = agents_text ++ before(after(agents, "present@@hl-text@@"), "@@hl-more@@")',
        '    agents_more = contains(agents, "@@hl-more@@")',
        '    agents_skip = agents_skip ++ "1"',
        "  }",
        "}",
    ]
    return out


CLAUDE_MODELS_128K = [
    "claude-opus-5-5", "claude-fable-5-5", "claude-fable-5-2", "claude-mythos-5-2",
    "claude-fable-5-1", "claude-mythos-5-1", "claude-sonnet-5-5", "claude-haiku-5-5",
    "claude-opus-5", "claude-sonnet-5", "claude-opus-4-6", "claude-sonnet-4-6",
]
CLAUDE_MODELS_64K = [
    "claude-haiku-4-5", "claude-haiku-4-5-20251001", "claude-sonnet-4-5",
    "claude-sonnet-4-5-20250929", "claude-opus-4-5", "claude-opus-4-5-20251101",
]
CLAUDE_ADAPTIVE = [
    "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "claude-fable-5-5",
    "claude-fable-5-2", "claude-mythos-5-2", "claude-fable-5-1", "claude-mythos-5-1",
    "claude-opus-5", "claude-sonnet-5", "claude-opus-4-6", "claude-sonnet-4-6",
]


def claude_tools(side):
    tools = json.loads(read("tools.json"))
    tools += json.loads(read("subagent-tools.json"))[side]
    tools[-1]["cache_control"] = {"type": "ephemeral"}
    return J(tools)


BLOCK = "{" + J("type") + ":" + J("text") + "," + J("text") + ":" + Q


def claude_generated(side, levels):
    variants = " | ".join(L(name) for name in CLAUDE_MODELS_128K + CLAUDE_MODELS_64K)
    out = ["type claude_model = " + variants, "", "fn model_table(model_name: claude_model) {", "  adaptive = false", "  match model_name {"]
    for name in CLAUDE_MODELS_128K + CLAUDE_MODELS_64K:
        tokens = "128000" if name in CLAUDE_MODELS_128K else "64000"
        out.append(f"    {L(name)} => {{")
        out.append(f"      model_id = {L(name)}")
        out.append(f"      max_tokens_text = {L(tokens)}")
        if name in CLAUDE_ADAPTIVE:
            out.append("      adaptive = true")
        out.append("    }")
    out += ["  }", "}", ""]
    description = L("Read the environment for the system prompt")
    out += probe_fns(lambda command: f"Bash({command}, probe_timeout_ms, {description}, false, false)", True, False)
    otherwise = "repair_history(error)" if side == "parent" else 'return failed("provider rejected the request: " ++ error)'
    out += [""] + login_fns(lambda command: f"Bash({command}, probe_timeout_ms, {L('Refresh the Claude login')}, false, false)", False, otherwise)
    out.append("")
    system = J(json.loads(read("claude-code-system.json")))
    assert system.startswith("[") and system.endswith("]")
    tools = claude_tools(side)
    environment = Jstr(NL.join(["# Environment", "You have been invoked in the following environment: ", "Primary working directory: "]))
    head_open = "{" + J("model") + ":" + Q
    messages_key = "]," + J("tools") + ":" + tools + "," + J("messages") + ":"
    out += [
        "fn compose() {",
        '  if login_route == "subscription" {',
        f"    url = {L(CLAUDE_MESSAGES_URL)}",
        f"    headers = {L(J([['authorization', 'Bearer ${@']])[:-3])} ++ env_loginfile ++ {L('#claudeAiOauth.accessToken}' + Q + '],' + J(['anthropic-beta', CLAUDE_OAUTH_BETA]) + ',' + J(['anthropic-version', '2023-06-01']) + ',' + J(['content-type', 'application/json']) + ']')}",
        '  } else if login_route == "token" {',
        f"    url = {L(CLAUDE_MESSAGES_URL)}",
        f"    headers = {L(J([['authorization', 'Bearer ${CLAUDE_CODE_OAUTH_TOKEN}'], ['anthropic-beta', CLAUDE_OAUTH_BETA], ['anthropic-version', '2023-06-01'], ['content-type', 'application/json']]))}",
        "  } else {",
        f"    url = {L('${ANTHROPIC_BASE_URL:-https://api.anthropic.com}/v1/messages')}",
        f"    headers = {L(J([['x-api-key', '${ANTHROPIC_API_KEY}'], ['anthropic-version', '2023-06-01'], ['content-type', 'application/json']]))}",
        "  }",
        f"  scratch = {L(system[:-1] + ',' + BLOCK + environment)} ++ env_cwd ++ {L(Jstr(NL + 'Is a git repository: '))} ++ env_git ++ {L(Jstr(NL + 'Platform: '))} ++ env_platform",
        '  if env_shell != "" {',
        f"    scratch = scratch ++ {L(Jstr(NL + 'Shell: '))} ++ env_shell",
        "  }",
        '  if env_os != "" {',
        f"    scratch = scratch ++ {L(Jstr(NL + 'OS Version: '))} ++ env_os",
        "  }",
        f"  scratch = scratch ++ {L(Jstr(NL + 'Today' + chr(39) + 's date: '))} ++ env_date ++ {L(Q + '}')}",
        "  if agents_present {",
        f"    scratch = scratch ++ {L(',' + BLOCK + Jstr('# AGENTS.md instructions for '))} ++ env_cwd ++ {L(Jstr(NL * 2 + '<INSTRUCTIONS>' + NL))} ++ agents_text ++ {L(Jstr(NL + '</INSTRUCTIONS>') + Q + '}')}",
        "  }",
        f"  head = {L(head_open)} ++ model_id ++ {L(Q + ',' + J('max_tokens') + ':')} ++ max_tokens_text ++ {L(',' + J('system') + ':')} ++ scratch ++ {L(messages_key)}",
        f"  head_refresh = {L(head_open)} ++ model_id ++ {L(Q + ',' + J('max_tokens') + ':0,' + J('system') + ':')} ++ scratch ++ {L(messages_key)}",
        '  thinking("low")',
        f"  tail_low = {L(',' + J('stream') + ':true')} ++ scratch ++ {L('}')}",
        f"  tail_text_only = {L(',' + J('stream') + ':true,' + J('tool_choice') + ':' + J({'type': 'none'}))} ++ scratch ++ {L('}')}",
        '  scratch = ""',
        "}",
        "",
        "fn thinking(level: text) {",
        "  if adaptive {",
        f"    scratch = {L(',' + J('thinking') + ':' + J({'type': 'adaptive', 'display': 'summarized'}) + ',' + J('output_config') + ':{' + J('effort') + ':' + Q)} ++ level ++ {L(Q + '}')}",
        "  } else {",
        f"    scratch = {L(',' + J('thinking') + ':' + J({'type': 'disabled'}))}",
        "  }",
        "}",
    ]
    if levels:
        out += ["", "fn pin_tails() {", "  match pinned {"]
        for level in levels:
            out.append(f'    "{level}" => {{ thinking("{level}") }}')
        out += [
            "  }",
            f"  tail_main = {L(',' + J('stream') + ':true')} ++ scratch ++ {L('}')}",
            f"  tail_refresh = {L(',' + J('stream') + ':false')} ++ scratch ++ {L('}')}",
            '  scratch = ""',
            "}",
        ]
    return out


def codex_texts():
    return json.loads(read("codex-texts.json"))


def message_open():
    return "{" + J("type") + ":" + J("message") + "," + J("role") + ":" + J("user") + "," + J("content") + ":[{" + J("type") + ":" + J("input_text") + "," + J("text") + ":" + Q


def codex_generated(levels):
    texts = codex_texts()
    tools = J(json.loads(read("codex-tools.json")))
    instructions = J(read("codex-instructions.md"))
    reviewer = J(read("reviewer-system.md").rstrip())
    out = probe_fns(lambda command: f"exec_command({command}, {L('')}, probe_yield_ms, output_tokens_default, {L('')}, true, false)", True, True)
    out += [""] + login_fns(lambda command: f"exec_command({command}, {L('')}, refresh_yield_ms, output_tokens_default, {L('')}, true, false)", True, 'return failed("provider rejected the request: " ++ error)')
    head_open = "{" + J("model") + ":" + Q
    common = "," + J("store") + ":false," + J("stream") + ":true," + J("include") + ":" + J(["reasoning.encrypted_content"])
    verbosity = "," + J("text") + ":" + J({"verbosity": "low"})
    out += [
        "",
        "fn compose() {",
        '  if login_route == "subscription" {',
        f"    url = {L(CHATGPT_RESPONSES_URL)}",
        '  } else if env_base == "slash" {',
        f"    url = {L('${OPENAI_BASE_URL}v1/responses')}",
        '  } else if env_base == "v1" {',
        f"    url = {L('${OPENAI_BASE_URL}/responses')}",
        '  } else if env_base == "v1slash" {',
        f"    url = {L('${OPENAI_BASE_URL}responses')}",
        '  } else if env_base == "other" {',
        f"    url = env_normal ++ {L('/v1/responses')}",
        "  } else {",
        f"    url = {L('${OPENAI_BASE_URL:-https://api.openai.com}/v1/responses')}",
        "  }",
        '  if review_setting == "on" {',
        "    review_url = url",
        "  } else {",
        "    review_url = review_setting",
        "  }",
        '  if login_route == "subscription" {',
        f"    headers = {L(J([['authorization', 'Bearer ${@']])[:-3])} ++ env_loginfile ++ {L('#tokens.access_token}' + Q + '],' + J(['chatgpt-account-id', '${@'])[:-2])} ++ env_loginfile ++ {L('#tokens.account_id}' + Q + '],' + J([['content-type', 'application/json'], ['accept', 'text/event-stream']])[1:-1] + ',[' + J('session-id') + ',' + Q)} ++ cache_key ++ {L(Q + '],[' + J('thread-id') + ',' + Q)} ++ cache_key ++ {L(Q + ']]')}",
        "  } else {",
        f"    headers = {L(J([['authorization', 'Bearer ${OPENAI_API_KEY}'], ['content-type', 'application/json'], ['accept', 'text/event-stream']])[:-1] + ',[' + J('session-id') + ',' + Q)} ++ cache_key ++ {L(Q + '],[' + J('thread-id') + ',' + Q)} ++ cache_key ++ {L(Q + ']]')}",
        "  }",
        f"  head = {L(head_open)} ++ request_model ++ {L(Q + ',' + J('instructions') + ':' + instructions + ',' + J('input') + ':')}",
        f"  review_head = {L(head_open)} ++ request_model ++ {L(Q + ',' + J('instructions') + ':' + reviewer + ',' + J('input') + ':')}",
        '  if env_shell == "" {',
        '    env_shell = "bash"',
        "  }",
        '  preamble = ""',
        "  if agents_present {",
        f"    preamble = {L(message_open() + Jstr('# AGENTS.md instructions for '))} ++ env_cwd ++ {L(Jstr(NL * 2 + '<INSTRUCTIONS>' + NL))} ++ agents_text ++ {L(Jstr(NL + '</INSTRUCTIONS>') + Q + '}]},')}",
        "  }",
        f"  preamble = preamble ++ {L(message_open() + Jstr('<environment_context>' + NL + '  <cwd>'))} ++ env_cwd ++ {L(Jstr('</cwd>' + NL + '  <shell>'))} ++ env_shell ++ {L(Jstr('</shell>' + NL + '  <current_date>'))} ++ env_date ++ {L(Jstr('</current_date>' + NL + '  <timezone>'))} ++ env_zone ++ {L(Jstr('</timezone>' + NL + '</environment_context>') + Q + '}]}')}",
        "}",
        "",
        "fn pin_tails() {",
        "  match pinned {",
    ]
    for level in levels:
        out.append(f'    "{level}" => {{ scratch = "{level}" }}')
    out += [
        "  }",
        f"  tail = {L(',' + J('tools') + ':' + tools + ',' + J('tool_choice') + ':' + J('auto') + ',' + J('parallel_tool_calls') + ':true,' + J('reasoning') + ':{' + J('effort') + ':' + Q)} ++ scratch ++ {L(Q + '}' + common + ',' + J('prompt_cache_key') + ':' + Q)} ++ cache_key ++ {L(Q)}",
        f"  review_tail = {L(',' + J('tools') + ':' + tools + ',' + J('tool_choice') + ':' + J('none') + ',' + J('parallel_tool_calls') + ':false,' + J('reasoning') + ':{' + J('effort') + ':' + J('low') + '}' + common + ',' + J('max_output_tokens') + ':' + str(8000) + ',' + J('prompt_cache_key') + ':' + Q)} ++ cache_key ++ {L(Q)}",
        "  if verbose {",
        f"    tail = tail ++ {L(verbosity)}",
        f"    review_tail = review_tail ++ {L(verbosity)}",
        "  }",
        '  tail = tail ++ "}"',
        '  review_tail = review_tail ++ "}"',
        '  scratch = ""',
        "}",
        "",
        "fn finish_check() {",
        f"  next_input = {L(texts['FINISH_CHECK'])}",
        "}",
        "",
        "fn escalate_ask() {",
        f"  next_input = {L(texts['ESCALATE'])}",
        "}",
        "",
        "fn reviewed(review: text) {",
        f"  next_input = {L(texts['REVIEWED'] + NL * 2)} ++ review",
        "}",
        "",
        "fn review_ask() {",
        f"  scratch = {L('<task>' + NL)} ++ task_text ++ {L(NL + '</task>' + NL * 2 + 'The conversation above is the agent' + chr(39) + 's transcript for this task: its commands, their outputs and its messages. Judge it as your instructions say.')}",
        "}",
    ]
    return out


def policies(path):
    with open(path, encoding="utf-8") as file:
        lines = file.read().split(NL)
    for name, fixture in [("claude_policy", "claude-host.json"), ("codex_policy", "codex-host.json")]:
        prefix = "  let " + name + " = "
        at = next(i for i, line in enumerate(lines) if line.startswith(prefix))
        lines[at] = prefix + L(J(json.loads(read(fixture)))[1:-1])
    with open(path, "w", encoding="utf-8") as file:
        file.write(NL.join(lines))


def splice(path, fns):
    with open(path, encoding="utf-8") as file:
        lines = file.read().split(NL)
    blocks = []
    current = []
    for line in fns:
        if line == "":
            if current:
                blocks.append(current)
            current = []
        else:
            current.append(line)
    if current:
        blocks.append(current)
    for block in blocks:
        if block[0].startswith("type "):
            prefix = block[0][:block[0].index("=") + 1]
            at = next((i for i, line in enumerate(lines) if line.startswith(prefix)), None)
            if at is None:
                at = next(i for i, line in enumerate(lines) if line.startswith("type "))
                lines[at:at] = block
            else:
                lines[at] = block[0]
            continue
        name = re.match(r"fn ([a-z_]+)\(", block[0]).group(1)
        start = next((i for i, line in enumerate(lines) if line.startswith("fn " + name + "(")), None)
        if start is None:
            at = next(i for i, line in enumerate(lines) if line.startswith("fn "))
            lines[at:at] = block + [""]
        else:
            end = next(i for i in range(start, len(lines)) if lines[i] == "}")
            lines[start:end + 1] = block
    with open(path, "w", encoding="utf-8") as file:
        file.write(NL.join(lines))


if __name__ == "__main__":
    examples = ROOT + "/examples/"
    splice(examples + "claude_code.hl", claude_generated("parent", ["low", "medium", "high"]))
    splice(examples + "claude_code_child.hl", claude_generated("child", []))
    splice(examples + "codex_agent.hl", codex_generated(["medium", "low", "high", "xhigh"]))
    policies(examples + "entry.hl")
