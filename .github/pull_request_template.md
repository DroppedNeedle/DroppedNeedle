## What

## Why

## Testing

- [ ] I have tested these changes locally
- [ ] I have added or updated tests

## Did you use AI?

AI-assisted pull requests are welcome. If you used AI, tell us which tool and model, and roughly how you worked with it. We're curious what people use. See [Using AI](../CONTRIBUTING.md#using-ai).

## Checklist

- [ ] Server lint passes (`make server-lint`)
- [ ] Server tests pass (`make server-test`)
- [ ] Contract regenerated if a route or API type changed (`make contract-write`)
- [ ] Frontend lint passes (`make frontend-lint`)
- [ ] Frontend type check passes (`make frontend-check`)
- [ ] No `any` in TypeScript
- [ ] No hardcoded colors (design tokens only)
- [ ] Outbound HTTP uses a client from `server/kernel/src/http_client.rs`
- [ ] TanStack Query used for all new data fetching
- [ ] No secrets, tokens, or credentials in source
