use cinema_schema::{cinema_api, cinema_type};

use crate::app::{AppContext, Error};

#[cinema_type]
pub struct CollectionRequest {
    pub collection: String,
    pub media_type: String,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
}

#[cinema_type]
#[derive(sqlx::FromRow)]
pub struct CollectionItem {
    pub collection: String,
    pub media_type: String,
    pub tmdb_id: i64,
    pub title: String,
    pub poster_path: Option<String>,
    pub added_at: String,
    pub position: i64,
}

#[cinema_type]
pub struct CollectionStatus {
    pub in_collection: bool,
}

#[cinema_type]
#[derive(sqlx::FromRow)]
pub struct CollectionDef {
    pub slug: String,
    pub title: String,
    pub kind: String,
    pub created_at: String,
    // 0/1 flags (sqlite has no native bool; kept as ints for AnyPool safety).
    pub system: i64,
    pub hidden: i64,
}

#[cinema_type]
pub struct CreateCollection {
    pub slug: String,
    pub title: String,
    pub kind: String,
}

#[cinema_type]
pub struct ReorderItem {
    pub media_type: String,
    pub tmdb_id: i64,
}

#[cinema_api(namespace = "collections")]
pub trait CollectionsApi {
    /// Adds an item to a named collection
    async fn add(&self, item: CollectionRequest) -> Result<(), Error>;

    /// Removes an item from a collection
    async fn remove(&self, collection: String, media_type: String, id: i64) -> Result<(), Error>;

    /// Lists items in a single collection, ordered by position then added time
    async fn get(&self, collection: String) -> Result<Vec<CollectionItem>, Error>;

    /// Whether the given item is in the given collection
    async fn contains(
        &self,
        collection: String,
        media_type: String,
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

#[cinema_api]
impl CollectionsApi for AppContext {
    async fn add(&self, item: CollectionRequest) -> Result<(), Error> {
        sqlx::query(
            "INSERT INTO collections (collection, media_type, tmdb_id, title, poster_path)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(collection, media_type, tmdb_id) DO UPDATE SET title = excluded.title, poster_path = excluded.poster_path"
        )
        .bind(&item.collection)
        .bind(&item.media_type)
        .bind(item.tmdb_id)
        .bind(&item.title)
        .bind(&item.poster_path)
        .execute(&self.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(())
    }

    async fn remove(&self, collection: String, media_type: String, id: i64) -> Result<(), Error> {
        sqlx::query(
            "DELETE FROM collections WHERE collection = ? AND media_type = ? AND tmdb_id = ?",
        )
        .bind(&collection)
        .bind(&media_type)
        .bind(id)
        .execute(&self.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(())
    }

    async fn get(&self, collection: String) -> Result<Vec<CollectionItem>, Error> {
        let items = sqlx::query_as::<_, CollectionItem>(
            "SELECT collection, media_type, tmdb_id, title, poster_path, added_at, position
             FROM collections WHERE collection = ? ORDER BY position ASC, added_at DESC",
        )
        .bind(&collection)
        .fetch_all(&self.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(items)
    }

    async fn contains(
        &self,
        collection: String,
        media_type: String,
        id: i64,
    ) -> Result<CollectionStatus, Error> {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM collections WHERE collection = ? AND media_type = ? AND tmdb_id = ?",
        )
        .bind(&collection)
        .bind(&media_type)
        .bind(id)
        .fetch_one(&self.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(CollectionStatus {
            in_collection: count.0 > 0,
        })
    }

    async fn list_defs(&self) -> Result<Vec<CollectionDef>, Error> {
        let defs = sqlx::query_as::<_, CollectionDef>(
            "SELECT slug, title, kind, created_at, system, hidden FROM collection_meta ORDER BY position ASC, created_at ASC",
        )
        .fetch_all(&self.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(defs)
    }

    async fn create_def(&self, def: CreateCollection) -> Result<(), Error> {
        sqlx::query(
            "INSERT INTO collection_meta (slug, title, kind) VALUES (?, ?, ?)
             ON CONFLICT(slug) DO UPDATE SET title = excluded.title, kind = excluded.kind",
        )
        .bind(&def.slug)
        .bind(&def.title)
        .bind(&def.kind)
        .execute(&self.db)
        .await
        .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(())
    }

    async fn delete_def(&self, slug: String) -> Result<(), Error> {
        let system: Option<(i64,)> =
            sqlx::query_as("SELECT system FROM collection_meta WHERE slug = ?")
                .bind(&slug)
                .fetch_optional(&self.db)
                .await
                .map_err(|e| Error::Generic(e.to_string()))?;
        if matches!(system, Some((1,))) {
            return Err(Error::Generic(
                "system collections cannot be deleted".to_string(),
            ));
        }
        sqlx::query("DELETE FROM collection_meta WHERE slug = ?")
            .bind(&slug)
            .execute(&self.db)
            .await
            .map_err(|e| Error::Generic(e.to_string()))?;
        sqlx::query("DELETE FROM collections WHERE collection = ?")
            .bind(&slug)
            .execute(&self.db)
            .await
            .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(())
    }

    async fn set_visibility(&self, slug: String, hidden: bool) -> Result<(), Error> {
        sqlx::query("UPDATE collection_meta SET hidden = ? WHERE slug = ?")
            .bind(hidden as i64)
            .bind(&slug)
            .execute(&self.db)
            .await
            .map_err(|e| Error::Generic(e.to_string()))?;
        Ok(())
    }

    async fn reorder_defs(&self, slugs: Vec<String>) -> Result<(), Error> {
        for (idx, slug) in slugs.iter().enumerate() {
            sqlx::query("UPDATE collection_meta SET position = ? WHERE slug = ?")
                .bind(idx as i64)
                .bind(slug)
                .execute(&self.db)
                .await
                .map_err(|e| Error::Generic(e.to_string()))?;
        }
        Ok(())
    }

    async fn reorder(&self, collection: String, items: Vec<ReorderItem>) -> Result<(), Error> {
        for (idx, item) in items.iter().enumerate() {
            sqlx::query(
                "UPDATE collections SET position = ?
                 WHERE collection = ? AND media_type = ? AND tmdb_id = ?",
            )
            .bind(idx as i64)
            .bind(&collection)
            .bind(&item.media_type)
            .bind(item.tmdb_id)
            .execute(&self.db)
            .await
            .map_err(|e| Error::Generic(e.to_string()))?;
        }
        Ok(())
    }
}
