use anyhow::Context;
use calamine::{open_workbook_auto, Reader};
use poltergeist_core::tokens::DatabaseLookup;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct DatabaseRegistry {
    databases: HashMap<String, HashMap<String, HashMap<String, String>>>,
    columns: HashMap<String, Vec<String>>,
    names_original: HashMap<String, String>,
}

impl DatabaseRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn database_names(&self) -> Vec<String> {
        self.databases
            .keys()
            .map(|k| {
                self.names_original
                    .get(k)
                    .cloned()
                    .unwrap_or_else(|| k.clone())
            })
            .collect()
    }

    pub fn columns_of(&self, db_name: &str) -> Vec<String> {
        self.columns
            .get(&db_name.trim().to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    pub fn load_from_sources(
        &mut self,
        share_path: Option<&Path>,
        cache_path: Option<&Path>,
    ) -> anyhow::Result<()> {
        let candidates = discover_candidates(share_path, cache_path)?;
        let mut new_databases = HashMap::new();
        let mut new_columns = HashMap::new();
        let mut new_names = HashMap::new();

        for path in candidates {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let result = match ext.as_str() {
                "csv" => read_csv(&path),
                "xlsx" | "xlsm" => read_xlsx(&path),
                _ => continue,
            };
            let Ok((rows, columns)) = result else {
                continue;
            };
            let display_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if display_name.is_empty() {
                continue;
            }
            let lower_name = display_name.to_ascii_lowercase();
            new_names.insert(lower_name.clone(), display_name);
            new_columns.insert(lower_name.clone(), columns);
            new_databases.insert(lower_name, build_index(rows));
        }

        self.databases = new_databases;
        self.columns = new_columns;
        self.names_original = new_names;
        Ok(())
    }
}

impl DatabaseLookup for DatabaseRegistry {
    fn lookup(&self, db_name: &str, key: &str, column: Option<&str>) -> Option<String> {
        let db = self.databases.get(&db_name.trim().to_ascii_lowercase())?;
        let row = db.get(&key.trim().to_ascii_lowercase())?;
        let column = column.map(|c| c.trim().to_ascii_lowercase());
        match column {
            Some(column) => row.get(&column).cloned(),
            None => row.get("__key__").cloned(),
        }
    }
}

type Row = (String, HashMap<String, String>);

fn discover_candidates(
    share_path: Option<&Path>,
    cache_path: Option<&Path>,
) -> anyhow::Result<Vec<PathBuf>> {
    if let Some(share) = share_path {
        let share_db = share.join("databases");
        if share_db.is_dir() {
            let mut files = fs::read_dir(&share_db)
                .with_context(|| format!("failed to list {}", share_db.display()))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect::<Vec<_>>();
            files.sort();
            if !files.is_empty() {
                return Ok(files);
            }
        }
    }
    if let Some(cache) = cache_path {
        let cache_db = cache.join("databases");
        if cache_db.is_dir() {
            let mut files = fs::read_dir(&cache_db)
                .with_context(|| format!("failed to list {}", cache_db.display()))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect::<Vec<_>>();
            files.sort();
            return Ok(files);
        }
    }
    Ok(Vec::new())
}

fn build_index(rows: Vec<Row>) -> HashMap<String, HashMap<String, String>> {
    let mut index = HashMap::new();
    for (raw_key, row) in rows {
        let key_lower = raw_key.trim().to_ascii_lowercase();
        if key_lower.is_empty() || index.contains_key(&key_lower) {
            continue;
        }
        let mut entry = row;
        entry.insert("__key__".to_string(), raw_key);
        index.insert(key_lower, entry);
    }
    index
}

fn read_csv(path: &Path) -> anyhow::Result<(Vec<Row>, Vec<String>)> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .with_context(|| format!("failed to open csv {}", path.display()))?;
    let headers = reader
        .headers()
        .context("missing csv header")?
        .iter()
        .map(|h| h.trim().to_string())
        .collect::<Vec<_>>();

    let mut rows = Vec::new();
    for record in reader.records().flatten() {
        if record.is_empty() {
            continue;
        }
        let mut row = HashMap::new();
        for (idx, column) in headers.iter().enumerate() {
            let value = record.get(idx).unwrap_or_default().to_string();
            row.insert(column.to_ascii_lowercase(), value);
        }
        let key = record.get(0).unwrap_or_default().to_string();
        rows.push((key, row));
    }
    Ok((rows, headers))
}

fn read_xlsx(path: &Path) -> anyhow::Result<(Vec<Row>, Vec<String>)> {
    let mut workbook =
        open_workbook_auto(path).with_context(|| format!("failed to open {}", path.display()))?;
    let first_sheet = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("xlsx has no sheets"))?;
    let range = workbook
        .worksheet_range(&first_sheet)
        .with_context(|| format!("failed to read sheet {first_sheet}"))?;
    let mut rows_iter = range.rows();
    let headers = rows_iter
        .next()
        .ok_or_else(|| anyhow::anyhow!("xlsx has no header row"))?
        .iter()
        .map(|cell| cell.to_string().trim().to_string())
        .collect::<Vec<_>>();
    let mut rows = Vec::new();
    for cells in rows_iter {
        if cells.iter().all(|c| c.to_string().trim().is_empty()) {
            continue;
        }
        let mut row = HashMap::new();
        for (idx, header) in headers.iter().enumerate() {
            let value = cells.get(idx).map(|c| c.to_string()).unwrap_or_default();
            row.insert(header.to_ascii_lowercase(), value);
        }
        let key = cells.first().map(|c| c.to_string()).unwrap_or_default();
        rows.push((key, row));
    }
    Ok((rows, headers))
}
