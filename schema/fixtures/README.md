# Schema fixtures

Golden fixtures that each canonical event schema version must accept or reject. Each fixture is either `valid/` (schema MUST accept) or `invalid/` (schema MUST reject), with an accompanying `.notes.md` explaining the requirement it exercises.

Rule: fixture files never contain real customer data. Any string that could be interpreted as PII must be obviously synthetic.
