import type { components } from '$lib/api/v3/openapi';

type Schemas = components['schemas'];

export type DiscoverActivity = Schemas['DiscoverActivityRequest'];
export type RadioSeedItem = Schemas['RadioSeedItem'];

export type AlbumRef = Schemas['AlbumRef'];
export type ArtistRef = Schemas['ArtistRef'];
export type TrackRef = Schemas['TrackRef'];
export type GenreRef = Schemas['GenreRef'];
export type SectionItem = Schemas['SectionItem'];
export type SectionKind = Schemas['SectionKind'];
export type ChartSection = Schemas['ChartSection'];
export type ShelfSource = Schemas['ShelfSource'];
export type ListenSource = Schemas['ListenSource'];
export type StatsRange = Schemas['StatsRange'];
export type ServicePrompt = Schemas['ServicePrompt'];
export type IntegrationStatus = Schemas['IntegrationStatus'];
export type WeeklyExploration = Schemas['WeeklyExploration'];
export type NewReleaseSheet = Schemas['NewReleaseSheet'];
export type Gap = Schemas['Gap'];
export type OwnShelves = Schemas['OwnShelves'];
export type OwnShelfAlbum = Schemas['OwnShelfAlbum'];
export type WeeklyTrack = Schemas['WeeklyTrack'];
export type GenreArtwork = Schemas['GenreArtwork'];
export type GenreArtworkAlbum = Schemas['GenreArtworkAlbum'];
export type HomeResponse = Schemas['HomeResponse'];
export type DiscoverPreview = Schemas['DiscoverPreview'];
export type TeaserPick = Schemas['TeaserPick'];
export type YourLastfm = Schemas['YourLastfmResponse'];
export type DiscoverResponse = Schemas['DiscoverResponse'];
export type PageSection = Schemas['PageSection'];
export type SectionState = Schemas['SectionState'];
export type SectionCondition = Schemas['SectionCondition'];
export type SectionKey = Schemas['SectionKey'];
export type SectionZone = Schemas['SectionZone'];
export type GenreDetailResponse = Schemas['GenreDetailResponse'];
export type PlaylistSuggestionsResponse = Schemas['PlaylistSuggestionsResponse'];
export type RadioSeedKind = Schemas['RadioSeedKind'];
export type RadioPlanRequest = Schemas['RadioPlanRequest'];
export type RadioPlanTrack = Schemas['RadioPlanTrack'];
export type RadioPlanResponse = Schemas['RadioPlanResponse'];
export type PreviewTrackItem = Schemas['PreviewTrackItem'];
export type PreviewRole = Schemas['PreviewRole'];
export type PreviewSource = Schemas['PreviewSource'];
export type AlbumPreviewMode = Schemas['AlbumPreviewMode'];

/** A section row of one kind. */
export type SectionRow<K extends SectionItem['kind']> = Extract<SectionItem, { kind: K }>;
