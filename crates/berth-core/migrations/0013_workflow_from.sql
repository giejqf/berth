-- The workflow a workflow came from: one of its follow-ups, or a slot its
-- agent asked for (`request_slot`). The rail links the two (ADR 0027).
ALTER TABLE workflows ADD COLUMN from_workflow_id TEXT;
-- Follow-ups started before this column: the workflow whose summary or
-- outcome lists the follow-up.
UPDATE workflows SET from_workflow_id = (
    SELECT n.workflow_id FROM nodes n, json_each(n.payload_json, '$.follow_ups') f
    WHERE n.kind IN ('summary', 'outcome') AND n.workflow_id != workflows.id
      AND json_extract(f.value, '$.id') = json_extract(workflows.origin_json, '$.ref')
    ORDER BY n.created_at LIMIT 1)
WHERE json_extract(origin_json, '$.kind') = 'follow_up'
  AND json_extract(origin_json, '$.ref') != '';
