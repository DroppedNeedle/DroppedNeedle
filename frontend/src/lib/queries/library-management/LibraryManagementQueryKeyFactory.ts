import type { LibraryManagementHistoryParams, LibraryManagementPlanItemParams } from './types';

const userSegment = (userId: string | null | undefined) => userId ?? 'anonymous';

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

export const LibraryManagementQueryKeyFactory = {
	prefix: ['library-management'] as const,
	user: (userId: string | null | undefined) =>
		[...LibraryManagementQueryKeyFactory.prefix, userSegment(userId)] as const,
	settings: (userId: string | null | undefined) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'settings'] as const,
	profile: (userId: string | null | undefined, profileId: string) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'profiles', profileId] as const,
	presetDiff: (userId: string | null | undefined, profileId: string) =>
		[...LibraryManagementQueryKeyFactory.profile(userId, profileId), 'preset-diff'] as const,
	tagEditor: (userId: string | null | undefined, trackId: string) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'tag-editor', trackId] as const,
	previewsPrefix: (userId: string | null | undefined) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'previews'] as const,
	preview: (userId: string | null | undefined, jobId: string) =>
		[...LibraryManagementQueryKeyFactory.previewsPrefix(userId), jobId] as const,
	previewItems: (
		userId: string | null | undefined,
		jobId: string,
		params: LibraryManagementPlanItemParams
	) =>
		[
			...LibraryManagementQueryKeyFactory.preview(userId, jobId),
			'items',
			normalizedItems(params)
		] as const,
	activationPreview: (userId: string | null | undefined, jobId: string) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'activation-previews', jobId] as const,
	activationHealth: (userId: string | null | undefined) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'activation-health'] as const,
	operationsPrefix: (userId: string | null | undefined) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'operations'] as const,
	operations: (userId: string | null | undefined, params: LibraryManagementHistoryParams) =>
		[
			...LibraryManagementQueryKeyFactory.operationsPrefix(userId),
			'history',
			normalizedHistory(params)
		] as const,
	operation: (userId: string | null | undefined, jobId: string) =>
		[...LibraryManagementQueryKeyFactory.operationsPrefix(userId), jobId] as const,
	operationResults: (userId: string | null | undefined, jobId: string, limit: number) =>
		[...LibraryManagementQueryKeyFactory.operation(userId, jobId), 'results', { limit }] as const,
	recovery: (userId: string | null | undefined) =>
		[...LibraryManagementQueryKeyFactory.user(userId), 'recovery'] as const
};
