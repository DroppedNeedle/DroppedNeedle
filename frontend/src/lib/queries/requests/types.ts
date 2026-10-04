import type { components } from '$lib/api/v3/openapi';

export type RequestItem = components['schemas']['RequestItem'];
export type ActiveRequestsResponse = components['schemas']['ActiveRequestsResponse'];
export type ActiveCountResponse = components['schemas']['ActiveCountResponse'];
export type RequestHistoryResponse = components['schemas']['HistoryResponse'];
export type RequestActionResponse = components['schemas']['ActionResponse'];
export type RequestCancelKind = 'album' | 'track';
export type ClearHistoryResponse = components['schemas']['ClearHistoryResponse'];
export type AlbumIntake = components['schemas']['AlbumIntake'];
export type IntakeResponse = components['schemas']['IntakeResponse'];
export type TrackIntake = components['schemas']['TrackIntake'];
export type TrackIntakeResponse = components['schemas']['TrackIntakeResponse'];
export type BatchIntake = components['schemas']['BatchIntake'];
export type BatchIntakeResponse = components['schemas']['BatchIntakeResponse'];
export type BatchCancelBody = components['schemas']['BatchCancelBody'];
export type BatchCancelResponse = components['schemas']['BatchCancelResponse'];
export type SyncResponse = components['schemas']['SyncResponse'];
