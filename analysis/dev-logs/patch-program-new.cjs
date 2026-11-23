// One-off patch: switch Program construction to Program::new so the
// lower-cased search text is pre-computed at index time.
const fs = require('fs');
const file = process.argv[2];
let t = fs.readFileSync(file, 'utf8');
const before = t;

const repls = [
  // Win32 .exe
  [`Some(Program {
                name,
                full_path: path.to_path_buf(),
                icon_path: Some(path.to_path_buf()),
                description: String::new(),
                args: None,
                source,
                enabled: true,
            })`,
   `Some(Program::new(
                name,
                path.to_path_buf(),
                Some(path.to_path_buf()),
                String::new(),
                None,
                source,
                true,
            ))`],
  // .lnk / .url
  [`Some(Program {
                name,
                full_path: path.to_path_buf(),
                icon_path: target.clone().or_else(|| Some(path.to_path_buf())),
                description: link
                    .as_ref()
                    .and_then(|l| l.description.clone())
                    .unwrap_or_default(),
                args: link.as_ref().and_then(|l| l.arguments.clone()),
                source,
                enabled: true,
            })`,
   `Some(Program::new(
                name,
                path.to_path_buf(),
                target.clone().or_else(|| Some(path.to_path_buf())),
                link.as_ref().and_then(|l| l.description.clone()).unwrap_or_default(),
                link.as_ref().and_then(|l| l.arguments.clone()),
                source,
                true,
            ))`],
  // App Paths
  [`Some(Program {
                        name,
                        full_path: path.clone(),
                        icon_path: Some(path),
                        description: desc.unwrap_or_default(),
                        args: None,
                        source: ProgramSourceKind::AppPaths,
                        enabled: true,
                    })`,
   `Some(Program::new(
                        name,
                        path.clone(),
                        Some(path),
                        desc.unwrap_or_default(),
                        None,
                        ProgramSourceKind::AppPaths,
                        true,
                    ))`],
  // Uninstall
  [`Some(Program {
                        name: display,
                        full_path: icon_path.clone().unwrap_or_else(|| PathBuf::from(&key)),
                        icon_path,
                        description: String::new(),
                        args: None,
                        source: ProgramSourceKind::Uninstall,
                        enabled: true,
                    })`,
   `Some(Program::new(
                        display,
                        icon_path.clone().unwrap_or_else(|| PathBuf::from(&key)),
                        icon_path,
                        String::new(),
                        None,
                        ProgramSourceKind::Uninstall,
                        true,
                    ))`],
];

let applied = 0;
for (const [from, to] of repls) {
  if (t.includes(from)) { t = t.replace(from, to); applied++; }
  else console.error('NOT FOUND:\n' + from.slice(0, 120));
}

// The cache loader must fill in the new field as well.
const cacheFrom = `            Program {
                name: p.name,
                full_path: PathBuf::from(p.full_path),
                icon_path: p.icon_path.map(PathBuf::from),
                description: p.description,
                args: p.args,
                source: source_from_str(&p.source),
                enabled: p.enabled,
            }`;
const cacheTo = `            Program::new(
                p.name,
                PathBuf::from(p.full_path),
                p.icon_path.map(PathBuf::from),
                p.description,
                p.args,
                source_from_str(&p.source),
                p.enabled,
            )`;
if (t.includes(cacheFrom)) { t = t.replace(cacheFrom, cacheTo); applied++; }
else console.error('cache loader NOT FOUND');

fs.writeFileSync(file, t, 'utf8');
console.log(`applied ${applied}/${repls.length + 1}; changed=${t !== before}`);
