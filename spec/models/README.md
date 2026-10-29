# Protocol models

Executable models used to check protocol designs before implementation. Analysis aids; not production code.

| Model | What it checks | Findings |
|---|---|---|
| `legacy_arq_check.py` | D-RATS stateful ARQ (and the ALP span rule) by exhaustive state exploration | `docs/phase-b/arq-model-findings.md` |

Run with Python 3.11+ (stdlib only), e.g. `python3 spec/models/legacy_arq_check.py --help`.
