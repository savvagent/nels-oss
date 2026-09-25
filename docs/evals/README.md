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

Each run makes one paid call per fixture (about 55) and writes `docs/evals/<date>-<provider>-<model>.md`. Commit that file.

To evaluate a non-default model, set the matching `LLM_MODEL_GEMINI`, `LLM_MODEL_OPENAI` or `LLM_MODEL_ANTHROPIC` for the run. The model name goes into the results file name.

## Keeping it honest

- The eval uses the production system prompt (`rag::build_system_instructions`), the production adapters (`llm::generate_json`), the same `USER MESSAGE: ...` framing and the same 30s timeout as chat. Don't give it a separate prompt.
- To change the model a provider uses (`LLM_MODEL_*`), re-run the eval first.
- A provider that fails the gate is removed from `ENABLED_BYO_PROVIDERS` in production. Users who already saved a key for it keep working (spec §6).
- `backend/evals/chat_actions.jsonl` must have at least one fixture for every action in the prompt, and no fixture may expect an action the prompt does not offer. A unit test enforces both.
- Some fixtures expect `NONE` on purpose. They cover the cases where the prompt tells the model to ask a question instead of acting: a new budget with no strategy (rule 2n), a dictated amount that could be dollars and cents run together (rule 1), an affordability check with no amount (rule 20d), a balance question with no category (rule 20c), and a complaint that isn't a bug report (rule 19).
- If you change a prompt rule, re-check the fixtures it touches before you re-run the eval.
