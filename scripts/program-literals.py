import json
import re
import sys

ROOT = sys.argv[1] if len(sys.argv) > 1 else "."
PROMPTS = ROOT + "/agent/prompts/"
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
    lines += [
        "fi",
        "printf '@@hl-agents@@'",
        "if [ -f AGENTS.md ] && [ -r AGENTS.md ]; then printf 'present@@hl-text@@'; e < AGENTS.md | tail -c +$((${#s} * k + 1)) | head -c \"$k\"; t=$(e < AGENTS.md | wc -c); if [ \"$t\" -gt $(((${#s} + 1) * k)) ]; then printf '@@hl-more@@'; fi; fi",
        "printf '@@hl-end@@'",
    ]
    return "c='", "'; s='", "'\n" + "\n".join(lines)

Q = chr(34)
NL = chr(10)


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
    names = ["cwd", "git", "platform", "shell", "os", "date"] + (["zone", "key", "base", "normal"] if codex else [])
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
    out.append("")
    system = J(json.loads(read("claude-code-system.json")))
    assert system.startswith("[") and system.endswith("]")
    tools = claude_tools(side)
    environment = Jstr(NL.join(["# Environment", "You have been invoked in the following environment: ", "Primary working directory: "]))
    head_open = "{" + J("model") + ":" + Q
    messages_key = "]," + J("tools") + ":" + tools + "," + J("messages") + ":"
    out += [
        "fn compose() {",
        f"  url = {L('${ANTHROPIC_BASE_URL:-https://api.anthropic.com}/v1/messages')}",
        f"  headers = {L(J([['x-api-key', '${ANTHROPIC_API_KEY}'], ['anthropic-version', '2023-06-01'], ['content-type', 'application/json']]))}",
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
    source = read(ROOT + "/agent/src/codex_host.rs")
    texts = {}
    for name in ["FINISH_CHECK", "REVIEWED"]:
        match = re.search(r"const " + name + r': &str = "((?:[^"\\]|\\.)*)";', source)
        texts[name] = json.loads(Q + match.group(1) + Q)
    return texts


def message_open():
    return "{" + J("type") + ":" + J("message") + "," + J("role") + ":" + J("user") + "," + J("content") + ":[{" + J("type") + ":" + J("input_text") + "," + J("text") + ":" + Q


def codex_generated(levels):
    texts = codex_texts()
    tools = J(json.loads(read("codex-tools.json")))
    instructions = J(read("codex-instructions.md"))
    reviewer = J(read("reviewer-system.md").rstrip())
    out = probe_fns(lambda command: f"exec_command({command}, {L('')}, probe_yield_ms, output_tokens_default, {L('')}, true, false)", True, True)
    head_open = "{" + J("model") + ":" + Q
    common = "," + J("store") + ":false," + J("stream") + ":true," + J("include") + ":" + J(["reasoning.encrypted_content"])
    verbosity = "," + J("text") + ":" + J({"verbosity": "low"})
    out += [
        "",
        "fn compose() {",
        '  if env_base == "slash" {',
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
        f"  headers = {L(J([['authorization', 'Bearer ${OPENAI_API_KEY}'], ['content-type', 'application/json'], ['accept', 'text/event-stream']])[:-1] + ',[' + J('session-id') + ',' + Q)} ++ cache_key ++ {L(Q + '],[' + J('thread-id') + ',' + Q)} ++ cache_key ++ {L(Q + ']]')}",
        f"  head = {L(head_open)} ++ model_id ++ {L(Q + ',' + J('instructions') + ':' + instructions + ',' + J('input') + ':')}",
        f"  review_head = {L(head_open)} ++ model_id ++ {L(Q + ',' + J('instructions') + ':' + reviewer + ',' + J('input') + ':')}",
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
        "fn reviewed(review: text) {",
        f"  next_input = {L(texts['REVIEWED'] + NL * 2)} ++ review",
        "}",
        "",
        "fn review_ask() {",
        f"  scratch = {L('<task>' + NL)} ++ task_text ++ {L(NL + '</task>' + NL * 2 + 'The conversation above is the agent' + chr(39) + 's transcript for this task: its commands, their outputs and its messages. Judge it as your instructions say.')}",
        "}",
    ]
    return out


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
