# OpenRouter prompt caching

Sense enables automatic prompt caching on its OpenRouter connection by sending
this field at the top level of every Chat Completions request:

```json
{
  "cache_control": { "type": "ephemeral" }
}
```

The field is also sent on requests after tool results and saved-session resume.
fx does not infer a model's provider from an `anthropic/` prefix or insert cache
breakpoints into individual message blocks. OpenRouter owns automatic breakpoint
placement and provider compatibility. Sending the field does not guarantee a
cache hit or a particular cost reduction.

## Configured gateways

The vendored fx accepts an optional `cache_control` field in a configured provider
definition. It is omitted by default, so other OpenAI-compatible connections keep
their existing request format. Enable it only for gateways that support this
extension. The setting applies to every model on that connection.

For example, a providers object passed through `FX_CONFIGURED_PROVIDERS` can
contain:

```json
{
  "openrouter": {
    "protocol": "openai-chat-completions",
    "base_url": "https://openrouter.ai/api/v1",
    "auth": { "type": "bearer", "env": "OPENROUTER_API_KEY" },
    "cache_control": { "type": "ephemeral" }
  }
}
```

Only `{ "type": "ephemeral" }` is accepted. Omit the setting to disable it for a
configured connection; `null`, other types, and extra fields such as `ttl` are
rejected. Sense supplies this setting itself, independently of profile defaults.

This requires the modified fx checkout. An older `SENSE_FX` override rejects the
new provider field. Rebuild the vendored binary with the repository's Zig 0.17
compiler before launching the updated Sense app:

```sh
(cd vendor/fx && ../../.toolchain/zig/zig build)
cargo app
```

## Verification

```sh
cargo test -p sense agent_launch::tests::openrouter_enables_automatic_prompt_caching
cd vendor/fx/tests/e2e
FX_SOUND=0 bun test configured-providers.test.ts
```

The deterministic tests use isolated profiles and local fake gateways. They
inspect the actual requests from the checkout's built fx binary, including tool
continuation and saved-session resume. They do not make billed OpenRouter calls
or establish live cache-hit behavior.
