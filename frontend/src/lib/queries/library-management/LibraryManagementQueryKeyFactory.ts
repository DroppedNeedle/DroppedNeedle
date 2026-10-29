import { key, userScope } from '../keys';
import type { LibraryManagementHistoryParams, LibraryManagementPlanItemParams } from './types';

const normalizedHistory = (params: LibraryManagementHistoryParams) => ({
	limit: params.limit ?? 50,
	origin: params.origin ?? null,
	profileId: params.profileId ?? null,
	rootId: params.rootId ?? null,
	state: params.state ?? null,
	mode: params.mode ?? null,
	createdFrom: params.createdFrom ?? null,
	createdTo: params.createdTo ?? null
});

const normalizedItems = (params: LibraryManagementPlanItemParams) => ({
	limit: params.limit ?? 100,
	eligibility: params.eligibility ?? null,
	reasonCode: params.reasonCode ?? null,
	rootId: params.rootId ?? null,
	artistId: params.artistId ?? null,
	albumId: params.albumId ?? null,
	audioFormat: params.audioFormat ?? null,
	collisionClass: params.collisionClass ?? null,
	hasPreservedValue: params.hasPreservedValue ?? null,
	hasRepresentationLoss: params.hasRepresentationLoss ?? null,
	changeKind: params.changeKind ?? null
});

type UserId = string | null | undefined;

export const LibraryManagementQueryKeyFactory = {
	user: (userId: UserId) => key(userScope(userId), 'library-management'),
	settings: (userId: UserId) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'settings'] as const,
	profile: (userId: UserId, profileId: string) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'profiles', profileId] as const,
	presetDiff: (userId: UserId, profileId: string) =>
		[...LibraryManagementQueryKeyFactory.profile(userId, profileId), 'preset-diff'] as const,
	tagEditor: (userId: UserId, trackId: string) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'tag-editor', trackId] as const,
	previewsPrefix: (userId: UserId) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'previews'] as const,
	preview: (userId: UserId, jobId: string) =>
		[...LibraryManagementQueryKeyFactory.previewsPrefix(userId), jobId] as const,
	previewItems: (userId: UserId, jobId: string, params: LibraryManagementPlanItemParams) =>
		[
			...LibraryManagementQueryKeyFactory.preview(userId, jobId),
			'items',
			normalizedItems(params)
		] as const,
	activationPreview: (userId: UserId, jobId: string) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'activation-previews', jobId] as const,
	activationHealth: (userId: UserId) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'activation-health'] as const,
	placement: (userId: UserId) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'placement'] as const,
	operationsPrefix: (userId: UserId) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'operations'] as const,
	operations: (userId: UserId, params: LibraryManagementHistoryParams) =>
		[
			...LibraryManagementQueryKeyFactory.operationsPrefix(userId),
			'history',
			normalizedHistory(params)
		] as const,
	operation: (userId: UserId, jobId: string) =>
		[...LibraryManagementQueryKeyFactory.operationsPrefix(userId), jobId] as const,
	operationResults: (userId: UserId, jobId: string, limit: number) =>
		[...LibraryManagementQueryKeyFactory.operation(userId, jobId), 'results', { limit }] as const,
	recovery: (userId: UserId) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'recovery'] as const,
	automaticSkipped: (userId: UserId) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'automatic-skipped'] as const,
	takeovers: (userId: UserId) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'takeovers'] as const
};
