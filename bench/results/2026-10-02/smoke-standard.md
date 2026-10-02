# Smoke test: `standard` (2026-10-02 01:59)

- Load time: 4.6 s
- VRAM: baseline 1440 MiB, loaded 9358 MiB, after checks 9366 MiB (model + context = 7926 MiB)

| Check | Result | Detail | Gen t/s |
| --- | --- | --- | --- |
| offload | PASS | 65/65 layers on GPU |  |
| auth | PASS | no-key status=401, CORS allow-origin for evil.example='http://nebula.invalid' |  |
| chat | PASS | answer='391' | 31.0 |
| reasoning | PASS | answer='No — 221 = 13 × 17.', reasoning_chars=340, finish=stop | 48.4 |
| tool_call | PASS | get_weather({"city": "Toronto", "unit": "celsius"}) | 46.7 |
| json_schema | PASS | parsed={"language": "Rust", "primes": [2, 3, 5, 7, 11]} | 38.1 |
