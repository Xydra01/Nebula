# Smoke test: `pq2mtp` (2026-10-04 02:38)

- Load time: 15.4 s
- VRAM: baseline 759 MiB, loaded 9480 MiB, after checks 9502 MiB (model + context = 8743 MiB)

| Check | Result | Detail | Gen t/s |
| --- | --- | --- | --- |
| offload | PASS | 66/66 layers on GPU |  |
| auth | PASS | no-key status=401, CORS allow-origin for evil.example='http://nebula.invalid' |  |
| chat | PASS | answer='391' | 46.2 |
| reasoning | PASS | answer='No — 221 = 13 × 17.', reasoning_chars=101, finish=stop | 78.1 |
| tool_call | PASS | get_weather({"city": "Toronto", "unit": "celsius"}) | 78.6 |
| json_schema | PASS | parsed={"language": "Rust", "primes": [2, 3, 5, 7, 11]} | 59.4 |
