# Validate D-A output: translated agent.cordis.yml files parse as YAML lists with
# the expected __jsExpr / disabled_expr node counts. Throwaway verification script.
import glob
import yaml

total = 0
for f in glob.glob(r"F:\RustProjects\dsh-rs\resources\agent-presets\*\agent.cordis.yml"):
    with open(f, encoding="utf-8") as fh:
        doc = yaml.safe_load(fh)
    assert isinstance(doc, list), f"{f}: top-level must be a list"
    cnt = {"__jsExpr": 0, "disabled_expr": 0}

    def walk(o):
        if isinstance(o, dict):
            for k, v in o.items():
                if k == "__jsExpr":
                    cnt["__jsExpr"] += 1
                if k == "disabled_expr":
                    cnt["disabled_expr"] += 1
                walk(v)
        elif isinstance(o, list):
            for i in o:
                walk(i)

    walk(doc)
    total += sum(cnt.values())
    name = f.split("\\")[-2]
    print(f"{name}: entries={len(doc)} jsExpr={cnt['__jsExpr']} disabled_expr={cnt['disabled_expr']}")

print("TOTAL_NODES", total)
