# pwcli Configuration

User configuration lives at:

```text
~/.pwcli/config.json
```

Only user-editable choices belong in this file. Runtime paths, skill roots, and
graph safety limits are fixed by pwcli:

- pwcli home: `~/.pwcli`
- skill root: `~/.agents/skills`
- graph max rounds: `100`

Provider internals are inferred from provider `protocol`:

- `openai`: OpenAI-compatible chat completions
- `nvidia`: NVIDIA OpenAI-compatible chat completions with NVIDIA defaults
- `anthropic`: Anthropic messages API

## Example

```json
{
  "provider": "my-local-provider",
  "model": "my-chat-model",
  "thinking": true,
  "context": {
    "max_input_tokens": 128000,
    "keep_recent_turns": 8
  },
  "providers": [
    {
      "name": "my-local-provider",
      "protocol": "openai",
      "base_url": "https://example.com/v1",
      "api_key": "sk-...",
      "models": [
        {
          "name": "my-chat-model",
          "supports_image_input": true,
          "supports_thinking": true,
          "is_image_generation": false,
          "max_input_tokens": 1000000,
          "max_output_tokens": 4096
        }
      ]
    },
    {
      "name": "nvidia",
      "protocol": "nvidia",
      "base_url": "https://integrate.api.nvidia.com/v1",
      "api_key": "...",
      "models": [
        {
          "name": "minimaxai/minimax-m2.7",
          "supports_image_input": true,
          "supports_thinking": false,
          "is_image_generation": false,
          "max_input_tokens": 196000,
          "max_output_tokens": 8192
        }
      ]
    }
  ]
}
```

Interactive commands:

```text
/providers
/provider my-local-provider
/models
/model my-chat-model
/thinking
/thinking off
/context
/context 196000
/compact
```

`thinking` is provider-neutral. pwcli translates it at runtime:

- OpenAI-compatible providers receive `enable_thinking`.
- NVIDIA providers receive `chat_template_kwargs.thinking_mode`.
- Anthropic providers receive a `thinking` block.
