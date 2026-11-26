CREATE TABLE IF NOT EXISTS ncu_school_dossiers (
    account_id TEXT PRIMARY KEY NOT NULL,
    dossier_json TEXT NOT NULL CHECK (json_valid(dossier_json)),
    verified_at_ms INTEGER NOT NULL
);
