-- Gemini 2.5 thinking models report a separate `thoughtsTokenCount` in
-- usageMetadata. These are billed output tokens but are NOT included in
-- candidatesTokenCount (our output_tokens), though they ARE included in
-- totalTokenCount. Track them in their own column so input + output + thinking
-- reconciles with total.
ALTER TABLE llm_usage ADD COLUMN IF NOT EXISTS thinking_tokens INT NOT NULL DEFAULT 0;
