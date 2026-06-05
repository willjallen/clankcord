mod job_payload_pre_v0_7;
mod v0_10_0;
mod v0_2_0;
mod v0_3_0;
mod v0_4_0;
mod v0_5_0;
mod v0_6_0;
mod v0_7_0;
mod v0_8_0;
mod v0_9_0;

use std::cmp::Ordering;

use super::store::TimelineStore;
use crate::Result;

const BASELINE_SCHEMA_VERSION_TEXT: &str = "0.1.0";
const CURRENT_SCHEMA_VERSION_TEXT: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedSchemaMigration {
    pub version: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SchemaVersion {
    major: u64,
    minor: u64,
    patch: u64,
}

#[derive(Debug, Clone, Copy)]
struct RegisteredMigration {
    version: SchemaVersion,
    version_text: &'static str,
    name: &'static str,
}

const REGISTERED_MIGRATIONS: &[RegisteredMigration] = &[
    RegisteredMigration {
        version: SchemaVersion::new(0, 2, 0),
        version_text: "0.2.0",
        name: "job payload blob envelope",
    },
    RegisteredMigration {
        version: SchemaVersion::new(0, 3, 0),
        version_text: "0.3.0",
        name: "generic runtime scope projections",
    },
    RegisteredMigration {
        version: SchemaVersion::new(0, 4, 0),
        version_text: "0.4.0",
        name: "database hard-cut performance contracts",
    },
    RegisteredMigration {
        version: SchemaVersion::new(0, 5, 0),
        version_text: "0.5.0",
        name: "policy-driven durable retention",
    },
    RegisteredMigration {
        version: SchemaVersion::new(0, 6, 0),
        version_text: "0.6.0",
        name: "job payload blob agent invocation metadata",
    },
    RegisteredMigration {
        version: SchemaVersion::new(0, 7, 0),
        version_text: "0.7.0",
        name: "job payload blob text response attachments",
    },
    RegisteredMigration {
        version: SchemaVersion::new(0, 8, 0),
        version_text: "0.8.0",
        name: "transcription source mux slots",
    },
    RegisteredMigration {
        version: SchemaVersion::new(0, 9, 0),
        version_text: "0.9.0",
        name: "durable transcription mux planner",
    },
    RegisteredMigration {
        version: SchemaVersion::new(0, 10, 0),
        version_text: "0.10.0",
        name: "voice status snapshot payload state",
    },
];

impl TimelineStore {
    pub async fn ensure_schema_migration_table(&self) -> Result<()> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS clankcord_schema_migrations (
              version TEXT PRIMARY KEY,
              name TEXT NOT NULL,
              applied_at_ms BIGINT NOT NULL,
              clankcord_version TEXT NOT NULL
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn run_pending_schema_migrations(&self) -> Result<Vec<AppliedSchemaMigration>> {
        self.ensure_schema_migration_table().await?;
        let current_binary_version = SchemaVersion::parse(CURRENT_SCHEMA_VERSION_TEXT)?;
        let durable_version = self.durable_schema_version().await?;
        if durable_version > current_binary_version {
            anyhow::bail!(
                "durable schema version {} is newer than clankcord binary version {}",
                durable_version.as_text(),
                CURRENT_SCHEMA_VERSION_TEXT
            );
        }
        let mut applied = Vec::new();
        let mut migrations = REGISTERED_MIGRATIONS.to_vec();
        migrations.sort_by_key(|migration| migration.version);
        for migration in migrations {
            if migration.version <= durable_version || migration.version > current_binary_version {
                continue;
            }
            self.apply_schema_migration(migration).await?;
            applied.push(AppliedSchemaMigration {
                version: migration.version_text.to_string(),
                name: migration.name.to_string(),
            });
        }
        Ok(applied)
    }

    async fn durable_schema_version(&self) -> Result<SchemaVersion> {
        let rows = sqlx::query("SELECT version FROM clankcord_schema_migrations")
            .fetch_all(&self.pool)
            .await?;
        let mut durable = SchemaVersion::parse(BASELINE_SCHEMA_VERSION_TEXT)?;
        for row in rows {
            let version_text: String = sqlx::Row::try_get(&row, "version")?;
            let version = SchemaVersion::parse(&version_text)?;
            durable = durable.max(version);
        }
        Ok(durable)
    }

    async fn apply_schema_migration(&self, migration: RegisteredMigration) -> Result<()> {
        let mut transaction = self.pool.begin().await?;
        match migration.version_text {
            "0.2.0" => v0_2_0::run(&mut transaction).await?,
            "0.3.0" => v0_3_0::run(&mut transaction).await?,
            "0.4.0" => v0_4_0::run(&mut transaction).await?,
            "0.5.0" => v0_5_0::run(&mut transaction).await?,
            "0.6.0" => v0_6_0::run(&mut transaction).await?,
            "0.7.0" => v0_7_0::run(&mut transaction).await?,
            "0.8.0" => v0_8_0::run(&mut transaction).await?,
            "0.9.0" => v0_9_0::run(&mut transaction).await?,
            "0.10.0" => v0_10_0::run(&mut transaction).await?,
            version => anyhow::bail!("unregistered schema migration implementation {version}"),
        }
        sqlx::query(
            r#"
            INSERT INTO clankcord_schema_migrations(
              version,
              name,
              applied_at_ms,
              clankcord_version
            )
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(migration.version_text)
        .bind(migration.name)
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(CURRENT_SCHEMA_VERSION_TEXT)
        .execute(transaction.as_mut())
        .await?;
        transaction.commit().await?;
        Ok(())
    }
}

impl SchemaVersion {
    const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        let mut parts = raw.split('.');
        let major = parse_version_part(parts.next(), raw, "major")?;
        let minor = parse_version_part(parts.next(), raw, "minor")?;
        let patch = parse_version_part(parts.next(), raw, "patch")?;
        if parts.next().is_some() {
            anyhow::bail!("invalid clankcord schema version {raw}");
        }
        Ok(Self::new(major, minor, patch))
    }

    fn as_text(self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl Ord for SchemaVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl PartialOrd for SchemaVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn parse_version_part(raw: Option<&str>, version: &str, label: &str) -> Result<u64> {
    let Some(raw) = raw else {
        anyhow::bail!("invalid clankcord schema version {version}: missing {label}");
    };
    Ok(raw
        .parse::<u64>()
        .map_err(|_| anyhow::anyhow!("invalid clankcord schema version {version}: bad {label}"))?)
}
