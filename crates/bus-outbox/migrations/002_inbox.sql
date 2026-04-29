CREATE TABLE IF NOT EXISTS eventbus_inbox (
    message_id    TEXT         NOT NULL,
    consumer      TEXT         NOT NULL,
    processed_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    status        TEXT         NOT NULL DEFAULT 'pending',
    PRIMARY KEY (consumer, message_id)
);
