CREATE OR REPLACE FUNCTION session_configs_immutable()
RETURNS TRIGGER AS $$
BEGIN
    RAISE EXCEPTION 'session_configs is append-only: % is not permitted', TG_OP;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER session_configs_no_update
    BEFORE UPDATE ON session_configs
    FOR EACH ROW EXECUTE FUNCTION session_configs_immutable();

CREATE TRIGGER session_configs_no_delete
    BEFORE DELETE ON session_configs
    FOR EACH ROW EXECUTE FUNCTION session_configs_immutable();
