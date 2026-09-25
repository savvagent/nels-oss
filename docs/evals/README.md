# Chat action evals

Nels ships a BYO-key provider only after it passes this eval (nels-oss#3).

The gate is:
- at least 95% of replies parse as Nels's chat JSON
- at least 90% of replies pick the right action

## Run

```bash
cd backend
EVAL_PROVIDER=openai    EVAL_API_KEY=sk-...     cargo test --bin backend live_chat_action_eval -- --ignored --nocapture
EVAL_PROVIDER=anthropic EVAL_API_KEY=sk-ant-... cargo test --bin backend live_chat_action_eval -- --ignored --nocapture
EVAL_PROVIDER=gemini    EVAL_API_KEY=AIza...    cargo test --bin backend live_chat_action_eval -- --ignored --nocapture
```

If `EVAL_PROVIDER` or `EVAL_API_KEY` is unset, the test prints a skip message and passes without calling anything, so a plain `cargo test -- --ignored` never spends money.

Each run makes one paid call per fixture (53 today) and writes `docs/evals/<date>-<provider>-<model>.md`. Commit that file. The file is rewritten after every call, so an interrupted run keeps what it already paid for. Only commit a file whose `Fixtures:` count equals the number of lines in `backend/evals/chat_actions.jsonl`; a partial run can show `Gate: PASS` on too few fixtures.

To evaluate a non-default model, set the matching `LLM_MODEL_GEMINI`, `LLM_MODEL_OPENAI` or `LLM_MODEL_ANTHROPIC` for the run. The model name goes into the results file name.

## Keeping it honest

- The eval uses the production system prompt (`rag::build_system_instructions`), the production adapters (`llm::generate_json`), the same `USER MESSAGE: ...` framing and the same 30s timeout as chat. Don't give it a separate prompt.
- To change the model a provider uses (`LLM_MODEL_*`), re-run the eval first.
- `ENABLED_BYO_PROVIDERS` fails closed: unset or blank offers no BYO provider. A provider is added to it in production only after its passing results file is committed here.
- A provider that later fails the gate is removed from `ENABLED_BYO_PROVIDERS`. Users who already saved a key for it keep working (spec §6).
- The scorer compares expected params at any depth, so nested params such as the `retirement_profile` fields of `SET_RETIREMENT_PROFILE` are checked, not just top-level ones.
- `backend/evals/chat_actions.jsonl` must have at least one fixture for every action in the prompt, and no fixture may expect an action the prompt does not offer. A unit test enforces both.
- Some fixtures expect `NONE` on purpose. They cover the cases where the prompt tells the model to ask a question instead of acting: a new budget with no strategy (rule 2n), a dictated amount that could be dollars and cents run together (rule 1), an affordability check with no amount (rule 20d), a balance question with no category (rule 20c), and a complaint that isn't a bug report (rule 19).
- If you change a prompt rule, re-check the fixtures it touches before you re-run the eval.

## Alternative actions

A fixture can list `also_accept` actions that production also handles correctly for that message. `set-category-limit` accepts `UPDATE_CATEGORY` as well as `CREATE_CATEGORY`: the prompt prescribes the upsert, but the `UPDATE_CATEGORY` arm honors a limit sent without a new name (#132). Params are still checked. Use this only when the chat handler really does produce the same result; never use it to excuse a wrong action.
