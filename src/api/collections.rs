use crate::app::{AppContext, Error};

pub(crate) use crate::tmdb;

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
    async fn add(&self, item: CollectionRequest) -> Result<(), Error>;

    /// Removes an item from a collection
    async fn remove(
        &self,
        collection: String,
        media_type: tmdb::MediaType,
        id: i64,
    ) -> Result<(), Error>;

    /// Lists items in a single collection, ordered by position then added time
    async fn get(&self, collection: String) -> Result<Vec<CollectionItem>, Error>;

    /// Whether the given item is in the given collection
    async fn contains(
        &self,
        collection: String,
        media_type: tmdb::MediaType,
        id: i64,
    ) -> Result<CollectionStatus, Error>;

    /// Lists every collection definition (user + system), ordered by position
    async fn list_defs(&self) -> Result<Vec<CollectionDef>, Error>;

    /// Creates (or upserts) a collection definition
    async fn create_def(&self, def: CreateCollection) -> Result<(), Error>;

    /// Deletes a collection definition and all its items. System defs can't
    /// be deleted
    async fn delete_def(&self, slug: String) -> Result<(), Error>;

    /// Hides/unhides a collection from the UI without deleting its items
    async fn set_visibility(&self, slug: String, hidden: bool) -> Result<(), Error>;

    /// Reorders the list of collection definitions
    async fn reorder_defs(&self, slugs: Vec<String>) -> Result<(), Error>;

    /// Reorders items within a single collection
    async fn reorder(&self, collection: String, items: Vec<ReorderItem>) -> Result<(), Error>;
}

#[draad::api]
impl CollectionsApi for AppContext {
    async fn add(&self, item: CollectionRequest) -> Result<(), Error> {
        sqlx::query!(
            "INSERT INTO collections (collection, media_type, tmdb_id, title, poster_path)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT(collection, media_type, tmdb_id) DO UPDATE SET title = excluded.title, poster_path = excluded.poster_path",
            item.collection,
            item.media_type as tmdb::MediaType,
            item.tmdb_id,
            item.title,
            item.poster_path,
        )
        .execute(&self.db)
        .await
        .map_err(Error::DatabaseError)?;

        Ok(())
    }

    async fn remove(
        &self,
        collection: String,
        media_type: tmdb::MediaType,
        id: i64,
    ) -> Result<(), Error> {
        sqlx::query!(
            "DELETE FROM collections WHERE collection = $1 AND media_type = $2 AND tmdb_id = $3",
            collection,
            media_type as tmdb::MediaType,
            id,
        )
        .execute(&self.db)
        .await
        .map_err(Error::DatabaseError)?;

        Ok(())
    }

    async fn get(&self, collection: String) -> Result<Vec<CollectionItem>, Error> {
        let items = sqlx::query_as!(
            CollectionItem,
            r#"SELECT
                collection,
                media_type as "media_type: tmdb::MediaType",
                tmdb_id,
                title,
                poster_path,
                added_at,
                position
            FROM collections
            WHERE collection = $1
            ORDER BY position ASC, added_at DESC"#,
            collection
        )
        .fetch_all(&self.db)
        .await
        .map_err(Error::DatabaseError)?;

        Ok(items)
    }

    async fn contains(
        &self,
        collection: String,
        media_type: tmdb::MediaType,
        id: i64,
    ) -> Result<CollectionStatus, Error> {
        let count = sqlx::query!(
            "SELECT COUNT(*) FROM collections WHERE collection = $1 AND media_type = $2 AND tmdb_id = $3",
        collection,
        media_type as tmdb::MediaType,
        id,
    )
        .fetch_one(&self.db)
        .await
        .map_err(Error::DatabaseError)?;

        Ok(CollectionStatus {
            in_collection: count.count.unwrap_or(0) > 0,
        })
    }

    async fn list_defs(&self) -> Result<Vec<CollectionDef>, Error> {
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
        .map_err(Error::DatabaseError)?;

        Ok(defs)
    }

    async fn create_def(&self, def: CreateCollection) -> Result<(), Error> {
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
        .map_err(Error::DatabaseError)?;

        Ok(())
    }

    async fn delete_def(&self, slug: String) -> Result<(), Error> {
        let system = sqlx::query!("SELECT system FROM collection_meta WHERE slug = $1", slug)
            .fetch_optional(&self.db)
            .await
            .map_err(Error::DatabaseError)?
            .map(|rec| rec.system)
            .unwrap_or(false);

        if system {
            return Err(Error::Generic(
                "system collections cannot be deleted".to_string(),
            ));
        }

        sqlx::query!("DELETE FROM collection_meta WHERE slug = $1", slug)
            .execute(&self.db)
            .await
            .map_err(Error::DatabaseError)?;

        sqlx::query!("DELETE FROM collections WHERE collection = $1", slug)
            .execute(&self.db)
            .await
            .map_err(Error::DatabaseError)?;

        Ok(())
    }

    async fn set_visibility(&self, slug: String, hidden: bool) -> Result<(), Error> {
        sqlx::query!(
            "UPDATE collection_meta SET hidden = $1 WHERE slug = $2",
            hidden,
            slug
        )
        .execute(&self.db)
        .await
        .map_err(Error::DatabaseError)?;

        Ok(())
    }

    async fn reorder_defs(&self, slugs: Vec<String>) -> Result<(), Error> {
        for (idx, slug) in slugs.iter().enumerate() {
            sqlx::query!(
                "UPDATE collection_meta SET position = $1 WHERE slug = $2",
                i32::try_from(idx).unwrap_or(i32::MAX),
                slug
            )
            .execute(&self.db)
            .await
            .map_err(Error::DatabaseError)?;
        }
        Ok(())
    }

    async fn reorder(&self, collection: String, items: Vec<ReorderItem>) -> Result<(), Error> {
        for (idx, item) in items.iter().enumerate() {
            sqlx::query!(
                "UPDATE collections
                SET position = $1
                WHERE collection = $2 AND media_type = $3 AND tmdb_id = $4",
                i32::try_from(idx).unwrap_or(i32::MAX),
                collection,
                item.media_type as tmdb::MediaType,
                item.tmdb_id,
            )
            .execute(&self.db)
            .await
            .map_err(Error::DatabaseError)?;
        }
        Ok(())
    }
}
