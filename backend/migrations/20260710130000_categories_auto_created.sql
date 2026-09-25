-- #376: mark categories born as a side-effect of logging/editing a transaction
-- (chat ADD_TRANSACTION find-or-create, chat EDIT_TRANSACTION find-or-create, and the
-- "create new" category-choice path). Explicit CREATE_CATEGORY (chat) and REST create_category
-- leave it false. Additive + backfilled to false: existing rows are treated as user-created and
-- are never auto-removed by the emptied-category cleanup.
ALTER TABLE categories
    ADD COLUMN IF NOT EXISTS auto_created BOOLEAN NOT NULL DEFAULT false;
