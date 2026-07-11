use crate::app::CinemaError;
use crate::{app::AppContext, tmdb::MediaType};

use crate::tmdb;

#[draad::ty]
pub struct CollectionRequest {
    pub collection: String,
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
}

#[draad::ty]
pub struct CollectionItem {
    pub collection: String,
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub added_at: chrono::DateTime<chrono::Utc>,
    pub position: i64,
}

#[draad::ty]
pub struct CollectionStatus {
    pub in_collection: bool,
}

#[draad::ty]
#[derive(sqlx::Type)]
#[sqlx(type_name = "collection_kind", rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum CollectionKind {
    Manual,
    Ordered,
}

#[draad::ty]
pub struct CollectionDef {
    pub slug: String,
    pub title: String,
    pub kind: CollectionKind,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub system: bool,
    pub hidden: bool,
}

#[draad::ty]
pub struct CreateCollection {
    pub slug: String,
    pub title: String,
    pub kind: CollectionKind,
}

#[draad::ty]
pub struct ReorderItem {
    pub media_type: tmdb::MediaType,
    pub tmdb_id: i64,
}

#[draad::api(namespace = "collections")]
pub trait CollectionsApi {
    /// Adds an item to a named collection
    #[post]
    async fn add(&self, item: CollectionRequest) -> Result<(), CinemaError>;

    /// Removes an item from a collection
    #[delete]
    async fn remove(
        &self,
        collection: String,
        media_type: String, // draad doesn't allow enums in DELETE
        id: i64,
    ) -> Result<(), CinemaError>;

    /// Lists items in a single collection, ordered by position then added time
    #[get]
    async fn get(&self, collection: String) -> Result<Vec<CollectionItem>, CinemaError>;

    /// Whether the given item is in the given collection
    #[get]
    async fn contains(
        &self,
        collection: String,
        media_type: String, // draad doesn't allow enums in GET
        id: i64,
    ) -> Result<CollectionStatus, CinemaError>;

    /// Lists every collection definition (user + system), ordered by position
    #[get]
    async fn list_defs(&self) -> Result<Vec<CollectionDef>, CinemaError>;

    /// Creates (or upserts) a collection definition
    #[put]
    async fn create_def(&self, def: CreateCollection) -> Result<(), CinemaError>;

    /// Deletes a collection definition and all its items. System defs can't
    /// be deleted
    #[delete]
    async fn delete_def(&self, slug: String) -> Result<(), CinemaError>;

    /// Hides/unhides a collection from the UI without deleting its items
    #[patch]
    async fn set_visibility(&self, slug: String, hidden: bool) -> Result<(), CinemaError>;

    /// Reorders the list of collection definitions
    #[patch]
    async fn reorder_defs(&self, slugs: Vec<String>) -> Result<(), CinemaError>;

    /// Reorders items within a single collection
    #[patch]
    async fn reorder(&self, collection: String, items: Vec<ReorderItem>)
    -> Result<(), CinemaError>;
}

#[draad::api]
impl CollectionsApi for AppContext {
    async fn add(&self, item: CollectionRequest) -> Result<(), CinemaError> {
        let mut tx = self.db.begin().await.map_err(CinemaError::DatabaseError)?;

        let media_id =
            crate::tmdb::MediaItem::ensure_exists(item.tmdb_id, item.media_type, &mut tx, self)
                .await?;

        sqlx::query!(
            "
                INSERT INTO collections (collection_slug, media_id)
                VALUES ($1, $2)
                ON CONFLICT (collection_slug, media_id) DO NOTHING
            ",
            item.collection,
            media_id,
        )
        .execute(&mut *tx)
        .await
        .map_err(CinemaError::DatabaseError)?;

        tx.commit().await.map_err(CinemaError::DatabaseError)?;
        Ok(())
    }

    async fn remove(
        &self,
        collection: String,
        media_type: String,
        id: i64,
    ) -> Result<(), CinemaError> {
        let media_type: tmdb::MediaType = serde_json::from_str(&media_type).map_err(|_| {
            CinemaError::InvalidInput(String::from("Invalid values passed for media_type"))
        })?;
        sqlx::query!(
            "
                DELETE FROM collections
                WHERE collection_slug = $1
                AND media_id = (SELECT id FROM media_items WHERE media_type = $2 AND tmdb_id = $3)",
            collection,
            media_type as tmdb::MediaType,
            id,
        )
        .execute(&self.db)
        .await
        .map_err(CinemaError::DatabaseError)?;

        Ok(())
    }

    async fn get(&self, collection: String) -> Result<Vec<CollectionItem>, CinemaError> {
        let items = sqlx::query_as!(
            CollectionItem,
            r#"SELECT
                c.collection_slug as "collection",
                mi.media_type as "media_type: tmdb::MediaType",
                mi.tmdb_id,
                mi.title,
                mi.poster_path,
                c.added_at,
                c.position
            FROM collections c
            JOIN media_items mi ON mi.id = c.media_id
            WHERE c.collection_slug = $1
            ORDER BY c.position ASC, c.added_at DESC"#,
            collection
        )
        .fetch_all(&self.db)
        .await
        .map_err(CinemaError::DatabaseError)?;

        Ok(items)
    }

    async fn contains(
        &self,
        collection: String,
        media_type: String,
        id: i64,
    ) -> Result<CollectionStatus, CinemaError> {
        let media_type = MediaType::try_from(media_type)?;
        let exists: Option<bool> = sqlx::query_scalar!(
            r#"SELECT EXISTS (
                SELECT 1 FROM collections c
                JOIN media_items mi ON mi.id = c.media_id
                WHERE c.collection_slug = $1 AND mi.media_type = $2 AND mi.tmdb_id = $3
            )"#,
            collection,
            media_type as tmdb::MediaType,
            id,
        )
        .fetch_one(&self.db)
        .await
        .map_err(CinemaError::DatabaseError)?;

        Ok(CollectionStatus {
            in_collection: exists.unwrap_or(false),
        })
    }

    async fn list_defs(&self) -> Result<Vec<CollectionDef>, CinemaError> {
        let defs = sqlx::query_as!(
            CollectionDef,
            r#"SELECT
                slug,
                title,
                kind as "kind: CollectionKind",
                created_at,
                system,
                hidden
            FROM collection_meta
            ORDER BY position ASC, created_at ASC"#,
        )
        .fetch_all(&self.db)
        .await
        .map_err(CinemaError::DatabaseError)?;

        Ok(defs)
    }

    async fn create_def(&self, def: CreateCollection) -> Result<(), CinemaError> {
        sqlx::query!(
            "INSERT INTO collection_meta (slug, title, kind)
            VALUES ($1, $2, $3)
            ON CONFLICT(slug) DO UPDATE SET
                title = excluded.title,
                kind = excluded.kind",
            def.slug,
            def.title,
            def.kind as CollectionKind,
        )
        .execute(&self.db)
        .await
        .map_err(CinemaError::DatabaseError)?;

        Ok(())
    }

    async fn delete_def(&self, slug: String) -> Result<(), CinemaError> {
        let system = sqlx::query!("SELECT system FROM collection_meta WHERE slug = $1", slug)
            .fetch_optional(&self.db)
            .await
            .map_err(CinemaError::DatabaseError)?
            .map(|rec| rec.system)
            .unwrap_or(false);

        if system {
            return Err(CinemaError::Generic(
                "system collections cannot be deleted".to_string(),
            ));
        }

        // ON DELETE CASCADE on collections.collection_slug removes membership rows.
        sqlx::query!("DELETE FROM collection_meta WHERE slug = $1", slug)
            .execute(&self.db)
            .await
            .map_err(CinemaError::DatabaseError)?;

        Ok(())
    }

    async fn set_visibility(&self, slug: String, hidden: bool) -> Result<(), CinemaError> {
        sqlx::query!(
            "UPDATE collection_meta SET hidden = $1 WHERE slug = $2",
            hidden,
            slug
        )
        .execute(&self.db)
        .await
        .map_err(CinemaError::DatabaseError)?;

        Ok(())
    }

    async fn reorder_defs(&self, slugs: Vec<String>) -> Result<(), CinemaError> {
        for (idx, slug) in slugs.iter().enumerate() {
            sqlx::query!(
                "UPDATE collection_meta SET position = $1 WHERE slug = $2",
                i32::try_from(idx).unwrap_or(i32::MAX),
                slug
            )
            .execute(&self.db)
            .await
            .map_err(CinemaError::DatabaseError)?;
        }
        Ok(())
    }

    async fn reorder(
        &self,
        collection: String,
        items: Vec<ReorderItem>,
    ) -> Result<(), CinemaError> {
        for (idx, item) in items.iter().enumerate() {
            sqlx::query!(
                "
                    UPDATE collections
                    SET position = $1
                    WHERE collection_slug = $2
                    AND media_id = (SELECT id FROM media_items WHERE media_type = $3 AND tmdb_id = $4)
                ",
                i32::try_from(idx).unwrap_or(i32::MAX),
                collection,
                item.media_type as tmdb::MediaType,
                item.tmdb_id,
            )
            .execute(&self.db)
            .await
            .map_err(CinemaError::DatabaseError)?;
        }
        Ok(())
    }
}
