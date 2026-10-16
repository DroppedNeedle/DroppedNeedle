// The filters on a management preview's file list. The page reads `params`
// for the plan-items query; the filter bar binds the fields.
import type {
	LibraryManagementPlanItemParams,
	ManagementChangeKind,
	ManagementEligibility
} from '$lib/queries/library-management/types';

export class PreviewFilters {
	eligibility = $state<ManagementEligibility | ''>('');
	reasonCode = $state('');
	rootId = $state('');
	artistId = $state('');
	artistLabel = $state('');
	albumId = $state('');
	albumLabel = $state('');
	catalogSearch = $state('');
	audioFormat = $state('');
	changeKind = $state<ManagementChangeKind | ''>('');
	collisionClass = $state('');
	hasPreservedValue = $state(false);
	hasRepresentationLoss = $state(false);

	params(limit: number): LibraryManagementPlanItemParams {
		return {
			limit,
			eligibility: this.eligibility || undefined,
			reasonCode: this.reasonCode || undefined,
			rootId: this.rootId || undefined,
			artistId: this.artistId || undefined,
			albumId: this.albumId || undefined,
			audioFormat: this.audioFormat || undefined,
			collisionClass: this.collisionClass || undefined,
			hasPreservedValue: this.hasPreservedValue || undefined,
			hasRepresentationLoss: this.hasRepresentationLoss || undefined,
			changeKind: this.changeKind || undefined
		};
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
		this.eligibility = '';
		this.reasonCode = '';
		this.rootId = '';
		this.clearArtist();
		this.clearAlbum();
		this.catalogSearch = '';
		this.audioFormat = '';
		this.changeKind = '';
		this.collisionClass = '';
		this.hasPreservedValue = false;
		this.hasRepresentationLoss = false;
	}
}
