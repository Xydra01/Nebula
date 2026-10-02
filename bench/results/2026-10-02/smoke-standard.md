# Smoke test: `standard` (2026-10-02 01:17)

- Load time: 4.1 s
- VRAM: baseline 1406 MiB, loaded 9334 MiB, after checks 9346 MiB (model + context = 7940 MiB)

| Check | Result | Detail | Gen t/s |
| --- | --- | --- | --- |
| offload | PASS | 65/65 layers on GPU |  |
| auth | PASS | no-key status=401, CORS allow-origin for evil.example='http://nebula.invalid' |  |
| chat | PASS | answer='391' | 32.1 |
| reasoning | PASS | answer='No, because 221 = 13 × 17.', reasoning_chars=146, finish=stop | 49.6 |
| tool_call | PASS | get_weather({"city": "Toronto", "unit": "celsius"}) | 47.0 |
| json_schema | PASS | parsed={"language": "Rust", "primes": [2, 3, 5, 7, 11]} | 24.0 |
