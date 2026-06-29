// separate from downloadStatus.ts so the lucide imports stay out of the lucide-free nav store
import type { ComponentType } from 'svelte';
import {
	AlertTriangle,
	Ban,
	CheckCircle,
	Clock,
	Download,
	Eye,
	Loader,
	RefreshCw,
	Search,
	Timer,
	XCircle
} from 'lucide-svelte';

import type { DerivedDownloadStatus, RetryDisplay } from './downloadStatus';

export interface DownloadStatusMeta {
	label: string;
	badgeClass: string;
	icon: ComponentType;
	/** pulse animation while the state is in-flight */
	pulse: boolean;
}

// Visual treatment per retry state (label is built in the badge - it carries the
// attempt count / countdown). Waiting + re-running pulse like other in-flight states;
// an exhausted failure uses the plain error style.
type RetryKind = NonNullable<RetryDisplay>['kind'];
export const retryBadgeConfig: Record<RetryKind, Omit<DownloadStatusMeta, 'label'>> = {
	scheduled: { badgeClass: 'badge-warning', icon: Timer, pulse: true },
	retrying: { badgeClass: 'badge-warning', icon: RefreshCw, pulse: true },
	failed_exhausted: { badgeClass: 'badge-error', icon: XCircle, pulse: false }
};

export const downloadStatusConfig: Record<DerivedDownloadStatus, DownloadStatusMeta> = {
	searching: { label: 'Searching', badgeClass: 'badge-ghost', icon: Search, pulse: true },
	awaiting_review: {
		label: 'Awaiting review',
		badgeClass: 'badge-warning',
		icon: Eye,
		pulse: false
	},
	queued: { label: 'Queued', badgeClass: 'badge-ghost', icon: Clock, pulse: false },
	downloading: { label: 'Downloading', badgeClass: 'badge-primary', icon: Download, pulse: true },
	processing: { label: 'Processing', badgeClass: 'badge-accent', icon: Loader, pulse: true },
	completed: { label: 'Completed', badgeClass: 'badge-success', icon: CheckCircle, pulse: false },
	partial: { label: 'Partial', badgeClass: 'badge-warning', icon: AlertTriangle, pulse: false },
	failed: { label: 'Failed', badgeClass: 'badge-error', icon: XCircle, pulse: false },
	cancelled: { label: 'Cancelled', badgeClass: 'badge-neutral', icon: Ban, pulse: false }
};
