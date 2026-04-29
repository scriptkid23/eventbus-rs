CREATE TABLE IF NOT EXISTS eventbus_sagas (
    saga_id     TEXT         PRIMARY KEY,
    saga_type   TEXT         NOT NULL,
    state       JSONB        NOT NULL,
    status      TEXT         NOT NULL DEFAULT 'running',
    created_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ  NOT NULL DEFAULT now(),
    version     INT          NOT NULL DEFAULT 0
);
