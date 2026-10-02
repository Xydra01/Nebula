# Smoke test: `standard` (2026-10-02 01:01)

- Load time: 5.4 s
- VRAM: baseline 1412 MiB, loaded 9330 MiB, after checks 9338 MiB (model + context = 7926 MiB)

| Check | Result | Detail | Gen t/s |
| --- | --- | --- | --- |
| offload | PASS | 65/65 layers on GPU |  |
| auth | PASS | no-key status=401, CORS allow-origin for evil.example='http://nebula.invalid' |  |
| chat | PASS | answer='391' | 35.1 |
| reasoning | PASS | answer='No. 221 = 13 × 17.', reasoning_chars=397, finish=stop | 49.8 |
| tool_call | PASS | get_weather({"city": "Toronto", "unit": "celsius"}) | 48.8 |
| json_schema | PASS | parsed={"language": "Rust", "primes": [2, 3, 5, 7, 11]} | 30.6 |
