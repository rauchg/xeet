# Live modality-gateway runs, 2026-11-16

Real requests through Vercel AI Gateway (`AI_GATEWAY_API_KEY` in the session),
made with `xeet gateway run --client ai-gateway`, the screenshot
`tests/fixtures/gateway/invoices.png`, the instruction "Look at this screenshot
and tell me which invoices are overdue.", and a real vision reader
(`--reader ai-gateway:openai/gpt-4o-mini`, granted `vision:read` by the host).
Target-model profiles came from the gateway's own model metadata
(`--model ai-gateway:MODEL`, `provider-reported`), except where
`wrong-host-config.json` deliberately claims image support.

| run | target model | result |
|---|---|---|
| 1 | deepseek/deepseek-v3.1 (text-only) | represented; read by gpt-4o-mini; accepted |
| 2 | deepseek/deepseek-v3.1, wrong host config | image rejected (`does not accept image input`); one fallback; reused the reading; accepted |
| 3 | openai/gpt-4o-mini (vision) | original image sent; accepted |
| 4 | meta/llama-3.1-8b, wrong host config | image rejected; the matched evidence (`doesn't support the image content block`) came from the gateway's routed provider attempt; one fallback; accepted |
| 5 | amazon/nova-micro (text-only) | reused run 1's reading; accepted |

Files: `N-*.record.json` (the gateway's run records: digests and sizes, no
content), `N-*.answer.txt` (the model's reply), and
`1-text-only.delivered-artifact-projection.txt` (the artifact the text-only
models received, as stored). The models are third-party and non-deterministic:
re-running gives different wording and possibly different readings.

Observed: the vision model transcribed the text correctly, including the cut-off
"Overd..." (marked uncertain), but its bounding boxes were uniformly about
85 px (84-86 px) above the real rows (INV-1041 is at y=146, the model said 60),
at self-reported confidence 1.0.
