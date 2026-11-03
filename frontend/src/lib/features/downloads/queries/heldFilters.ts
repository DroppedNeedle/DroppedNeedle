export interface HeldListFilters {
	releaseGroupMbid?: string;
	taskId?: string;
	all?: boolean;
}

export function heldListQuery(filters: HeldListFilters) {
	return {
		release_group_mbid: filters.releaseGroupMbid || undefined,
		task_id: filters.taskId || undefined,
		all: filters.all || undefined
	};
}
