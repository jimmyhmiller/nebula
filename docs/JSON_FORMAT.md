# Nebula graph JSON, version 1

Use JSON for scripts and integrations. [service-map.json](../examples/service-map.json) is a complete example.

```json
{
  "schema_version": 1,
  "nodes": [
    {"id": "api", "label": "API server", "attributes": {"team": "platform", "critical": true}},
    {"id": "db", "label": "Database", "attributes": {"instances": 3}},
    {"id": "unused", "label": "An isolated node"}
  ],
  "edges": [
    {"source": "api", "target": "db", "directed": true, "weight": 2.5,
     "type": "reads", "attributes": {"protocol": "SQL"}}
  ]
}
```

- `schema_version` is optional for compatibility; when supplied it must be `1`.
- Node IDs must be unique, nonempty strings or numbers. Numbers are converted to string IDs. Display labels default to the ID. IDs are not replaced by internal compact indices on export.
- Explicit nodes preserve isolated vertices. Endpoints not declared in `nodes` are created with their ID as their label; use an explicit node table when metadata completeness matters.
- `attributes` accepts JSON values, preserving number, boolean, null, string, array and object values. Additional nonreserved object fields are also captured as attributes.
- Edge endpoints refer to source IDs. `directed` defaults to the root's optional `directed` flag, otherwise false. `weight` defaults to 1 and must be finite/nonnegative. `type` defaults to `edges`.
- Self-loops and repeated/parallel edges are preserved. Unknown endpoints and invalid endpoints are distinct: valid new IDs create nodes; missing/non-string/non-number endpoints are errors.
- `links` is accepted as an alternative to `edges`. Edge arrays `[source, target]` and bare node IDs are accepted for compatibility.
- Existing `edge_types: [{"name":"calls", "color":[90,150,255], "edges":[["api","db"]]}]` files remain supported. Typed arrays take precedence over root `edges`/`links`; a root `directed` value applies unless an individual edge overrides it.
- Exported `type_colors` maps relationship names to packed RGBA8 values. Relationships without a type color are colored by endpoint nodes.

`nebula --file -` reads this JSON from stdin. No server or account is required.

A `.nebula` project is a separate versioned document format. Use **Save project** to author it, and graph JSON to integrate with other tools.
