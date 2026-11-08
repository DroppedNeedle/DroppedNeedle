// The filters on a management preview's file list. The page reads `params`
// for the plan-items query; the filter bar binds the fields and the list's
// tabs bind `scope`, which starts on the files that change.
import type {
	LibraryManagementPlanItemParams,
	ManagementChangeKind,
	ManagementEligibility,
	ManagementListScope
} from '$lib/features/library-management/queries/types';

export class PreviewFilters {
	scope = $state<ManagementListScope>('changing');
	#eligibility = $state<ManagementEligibility | ''>('');
	reasonCode = $state('');
	rootId = $state('');
	artistId = $state('');
	artistLabel = $state('');
	albumId = $state('');
	albumLabel = $state('');
	catalogSearch = $state('');
	audioFormat = $state('');
	#changeKind = $state<ManagementChangeKind | ''>('');

	// Blocked, stale and unchanged files are never on the Changing tab, so
	// picking one of them shows every file instead of an empty list.
	get eligibility(): ManagementEligibility | '' {
		return this.#eligibility;
	}

	set eligibility(value: ManagementEligibility | '') {
		this.#eligibility = value;
		if (value === 'blocked' || value === 'stale') this.#leaveChanging();
	}

	get changeKind(): ManagementChangeKind | '' {
		return this.#changeKind;
	}

	set changeKind(value: ManagementChangeKind | '') {
		this.#changeKind = value;
		if (value === 'no_change') this.#leaveChanging();
	}

	#leaveChanging(): void {
		if (this.scope === 'changing') this.scope = 'all';
	}

	params(limit: number): LibraryManagementPlanItemParams {
		return {
			limit,
			scope: this.scope === 'all' ? undefined : this.scope,
			eligibility: this.eligibility || undefined,
			reasonCode: this.reasonCode || undefined,
			rootId: this.rootId || undefined,
			artistId: this.artistId || undefined,
			albumId: this.albumId || undefined,
			audioFormat: this.audioFormat || undefined,
			changeKind: this.changeKind || undefined
		};
	}

	/** Whether any filter besides the tab narrows the list. */
	narrowed(): boolean {
		const { limit: _limit, scope: _scope, ...rest } = this.params(0);
		return Object.values(rest).some((value) => value !== undefined);
	}

	chooseArtist(id: string, label: string): void {
		this.artistId = id;
		this.artistLabel = label;
		this.catalogSearch = '';
	}

	chooseAlbum(id: string, label: string): void {
		this.albumId = id;
		this.albumLabel = label;
		this.catalogSearch = '';
	}

	clearArtist(): void {
		this.artistId = '';
		this.artistLabel = '';
	}

	clearAlbum(): void {
		this.albumId = '';
		this.albumLabel = '';
	}

	clear(): void {
		this.scope = 'changing';
		this.#eligibility = '';
		this.reasonCode = '';
		this.rootId = '';
		this.clearArtist();
		this.clearAlbum();
		this.catalogSearch = '';
		this.audioFormat = '';
		this.#changeKind = '';
	}
}
