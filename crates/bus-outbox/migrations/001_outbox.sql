CREATE TABLE IF NOT EXISTS eventbus_outbox (
    id              UUID         PRIMARY KEY,
    aggregate_type  TEXT         NOT NULL,
    aggregate_id    TEXT         NOT NULL,
    subject         TEXT         NOT NULL,
    payload         JSONB        NOT NULL,
    headers         JSONB        NOT NULL DEFAULT '{}',
    created_at      TIMESTAMPTZ  NOT NULL DEFAULT now(),
    published_at    TIMESTAMPTZ  NULL,
    attempts        INT          NOT NULL DEFAULT 0,
    last_error      TEXT         NULL
);

CREATE INDEX IF NOT EXISTS eventbus_outbox_pending_idx
    ON eventbus_outbox (created_at)
    WHERE published_at IS NULL;
