-- Who talks to the bot (D59).
--
-- A Telegram user gets an account of their own the first time the bot hears of them:
-- a client's, known by the id Telegram gives them. It has no address and no password,
-- so nobody can log in to it with either. The bot is given a token for it by showing
-- the API a key (D60).
--
-- An account that was opened with an address keeps its address and its password, and
-- has no Telegram id.

ALTER TABLE users
    -- The id Telegram gives a user. It has at most 52 significant bits.
    ADD COLUMN telegram_id BIGINT,
    ALTER COLUMN email DROP NOT NULL,
    ALTER COLUMN password_hash DROP NOT NULL;

ALTER TABLE users
    -- A Telegram user is one account. The accounts that have no Telegram id are as
    -- many as they are: a NULL equals no other NULL.
    ADD CONSTRAINT users_telegram_id_key UNIQUE (telegram_id),
    ADD CONSTRAINT users_telegram_id_check CHECK (telegram_id > 0),
    -- A password belongs to an address: an account has both or neither.
    ADD CONSTRAINT users_password_check CHECK ((email IS NULL) = (password_hash IS NULL)),
    -- And every account can be reached one way or the other.
    ADD CONSTRAINT users_reachable_check CHECK (email IS NOT NULL OR telegram_id IS NOT NULL);
