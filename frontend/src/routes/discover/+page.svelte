<script lang="ts">
	import { untrack } from 'svelte';
	import { prefersReducedMotion } from 'svelte/motion';
	import {
		ArrowRight,
		CircleAlert,
		Compass,
		LayoutGrid,
		Library,
		Music,
		SlidersHorizontal,
		Sparkles
	} from 'lucide-svelte';
	import HomeSection from '$lib/features/discover/components/HomeSection.svelte';
	import ServiceErrorState from '$lib/ui/ServiceErrorState.svelte';
	import DailyMixCard from '$lib/features/discover/components/DailyMixCard.svelte';
	import RadioCard from '$lib/features/discover/components/RadioCard.svelte';
	import PlayableSection from '$lib/features/discover/components/PlayableSection.svelte';
	import JumpTabs from '$lib/ui/JumpTabs.svelte';
	import DiscoverPlaylistBand from '$lib/features/discover/components/DiscoverPlaylistBand.svelte';
	import SectionHead from '$lib/ui/SectionHead.svelte';
	import GenreGrid from '$lib/features/genre/components/GenreGrid.svelte';
	import ToldDiscoverModal from '$lib/features/discover/components/ToldDiscoverModal.svelte';
	import PlaylistDiscoveryModal from '$lib/features/discover/components/PlaylistDiscoveryModal.svelte';
	import WeeklyExploration from '$lib/features/discover/components/WeeklyExploration.svelte';
	import SectionDivider from '$lib/ui/SectionDivider.svelte';
	import CarouselSkeleton from '$lib/ui/CarouselSkeleton.svelte';
	import Button from '$lib/ui/Button.svelte';
	import Card from '$lib/ui/Card.svelte';
	import Tape from '$lib/ui/Tape.svelte';
	import WeekShelf from '$lib/ui/WeekShelf.svelte';
	import AlbumImage from '$lib/media/AlbumImage.svelte';
	import { fly } from '$lib/ui/motion';
	import { withBasePath } from '$lib/utils/basePath';
	import SectionStateLine from '$lib/features/discover/components/SectionStateLine.svelte';
	import WeekHeader from '$lib/features/discover/components/WeekHeader.svelte';
	import DiscoverPageLinks from '$lib/features/discover/components/DiscoverPageLinks.svelte';
	import Counter from '$lib/features/discover/components/Counter.svelte';
	import PickCard from '$lib/features/discover/components/PickCard.svelte';
	import PickCardSkeleton from '$lib/features/discover/components/PickCardSkeleton.svelte';
	import PickActionBar from '$lib/features/discover/components/PickActionBar.svelte';
	import WeekBin from '$lib/features/discover/components/WeekBin.svelte';
	import ListeningPost from '$lib/features/discover/components/ListeningPost.svelte';
	import WeekReceipt from '$lib/features/discover/components/WeekReceipt.svelte';
	import WhyCard from '$lib/features/discover/components/WhyCard.svelte';
	import AfterNoSlip from '$lib/features/discover/components/AfterNoSlip.svelte';
	import NewFromYourArtists from '$lib/features/discover/components/NewFromYourArtists.svelte';
	import PastWeeksRow from '$lib/features/discover/components/PastWeeksRow.svelte';
	import GapsShelf from '$lib/features/discover/components/GapsShelf.svelte';
	import FromYourShelves from '$lib/features/discover/components/FromYourShelves.svelte';
	import StatedArtists from '$lib/features/discover/components/StatedArtists.svelte';
	import { getStatedArtistsQuery } from '$lib/features/discover/queries/statedArtists.svelte';
	import { getDiscoverHomeV3Query } from '$lib/features/discover/queries/DiscoverV3Queries.svelte';
	import { createPickEventMutation } from '$lib/features/discover/queries/DiscoverV3Mutations.svelte';
	import { useDiscoverActivity } from '$lib/features/discover/queries/DiscoverDemand.svelte';
	import {
		createKeepPickMutation,
		getWeekMoreQuery,
		getWeekQuery,
		quotaRefusal
	} from '$lib/features/discover/queries/week.svelte';
	import {
		createMarkLaterMutation,
		createRemoveLaterMutation,
		getLaterPileQuery
	} from '$lib/features/discover/queries/later.svelte';
	import {
		createWeekDeck,
		lookOf,
		type DeckEntry,
		type PickState
	} from '$lib/features/discover/state/weekDeck.svelte';
	import { passOn, pickSlip } from '$lib/features/discover/state/passLine.svelte';
	import { pickAnswers } from '$lib/features/discover/state/pickAnswers.svelte';
	import { deckSampler, type SampleEntry } from '$lib/features/discover/state/deckSampler.svelte';
	import { audioFocus } from '$lib/features/player/state/audioFocus.svelte';
	import { playerStore } from '$lib/features/player/state/player.svelte';
	import { dayDate, longDayDate } from '$lib/features/discover/state/weekWords';
	import { authStore } from '$lib/stores/authStore.svelte';
	import type {
		ChartSection,
		Gap,
		NewReleaseSheet,
		OwnShelves,
		PageSection,
		SectionKey,
		SectionState,
		SectionZone
	} from '$lib/features/discover/queries/types';
	import type { KitIcon } from '$lib/ui/Button.svelte';
	import type { StateLook } from '$lib/ui/StatusChip.svelte';
	import { getSectionPrefsQuery } from '$lib/features/settings/queries/section-prefs.svelte';
	import {
		gapsOf,
		newReleasesOf,
		ownShelvesOf,
		shelfOf,
		shelvesOf,
		stateOf,
		weeklyOf
	} from '$lib/features/discover/queries/sections';
	import { albumRows, genreRows } from '$lib/features/discover/components/sectionRows';
	import type { PageProps } from './$types';

	const { data }: PageProps = $props();

	// The page ends with one way to the charts, on the person's own source.
	const popularOn = $derived(data.primarySource === 'lastfm' ? 'Last.fm' : 'ListenBrainz');

	useDiscoverActivity(() => ({ feature: 'discover' }));

	let playlistDiscoverOpen = $state(false);
	let toldOpen = $state(false);
	let whyOpen = $state(false);

	// ---------- the week on the counter ----------

	const discoverQuery = getDiscoverHomeV3Query();
	const setEnabled = $derived(discoverQuery.data?.weekly_set_enabled ?? true);
	const weekQuery = getWeekQuery(() => setEnabled);
	const laterQuery = getLaterPileQuery();
	const week = $derived(weekQuery.data ?? null);
	const status = $derived(week?.status ?? null);
	// The set on the counter: this week's, or last week's while it is kept.
	const set = $derived(
		status === 'ready' || status === 'kept_last_week' ? (week?.set ?? null) : null
	);
	const laterCount = $derived(laterQuery.data?.total ?? laterQuery.data?.items.length ?? 0);

	const deck = createWeekDeck();
	const allAnswered = $derived(!!set && deck.answered === set.picks.length);
	const moreQuery = getWeekMoreQuery(() => allAnswered);
	const offered = $derived(moreQuery.data?.pages.flatMap((page) => page.items) ?? []);
	const moreSettled = $derived(moreQuery.isSuccess || moreQuery.isError);

	$effect(() => {
		const picks = set?.picks ?? [];
		const extras = offered;
		untrack(() => deck.sync(picks, extras));
	});

	const current = $derived(deck.current);
	const done = $derived(deck.done && moreSettled);
	const tastes = $derived(set?.tastes ?? []);
	const linked = $derived(set?.counts.linked ?? true);

	const looks = $derived(
		new Map<string, StateLook | undefined>(
			deck.entries.map((entry) => [entry.key, lookOf(deck.stateOf(entry))])
		)
	);
	const spines = $derived(
		deck.entries
			.filter((entry) => entry.kind === 'pick')
			.map((entry) => ({ key: entry.key, title: titleOf(entry), look: looks.get(entry.key) }))
	);

	// ---------- naming artists on the empty counter ----------

	/** Named artists a first set needs. */
	const FIRST_SET = 3;
	const statedQuery = getStatedArtistsQuery();
	const statedCount = $derived(statedQuery.data?.items.length ?? 0);
	// Once the empty card has shown the picker it stays until three are
	// named, though the first name already has the week being set out.
	let naming = $state(false);
	$effect(() => {
		if (status === 'nothing_yet') naming = true;
	});
	const holdPicker = $derived(
		!set &&
			(status === 'nothing_yet' ||
				(status === 'being_set_out' && naming && statedCount < FIRST_SET))
	);

	const pad = (n: number) => String(n).padStart(2, '0');
	const titleOf = (entry: DeckEntry) =>
		entry.kind === 'pick' ? entry.pick.album.title : entry.extra.album.title;
	const albumOf = (entry: DeckEntry) =>
		entry.kind === 'pick' ? entry.pick.album : entry.extra.album;
	const reasonOf = (entry: DeckEntry) =>
		entry.kind === 'pick' ? entry.pick.reason : entry.extra.reason;
	const TAPES = { your_artists: 'New to you', source: 'ListenBrainz pick' } as const;

	function tapesOf(entry: DeckEntry): [string, string?] {
		const first = linked ? TAPES[reasonOf(entry).kind] : 'Picked from your shelves';
		if (entry.kind === 'extra') return [first, 'Extra'];
		const further = entry.pick.taste_no === null || entry.pick.taste_no === undefined;
		return further ? [first, 'Further out'] : [first];
	}

	function kickerOf(entry: DeckEntry): string {
		if (entry.kind === 'extra') return `Extra ${entry.n}`;
		const taste =
			entry.pick.taste_no !== null && entry.pick.taste_no !== undefined
				? tastes[entry.pick.taste_no]
				: undefined;
		return taste
			? `No. ${pad(entry.pick.pick_no)} - ${taste.name}`
			: `No. ${pad(entry.pick.pick_no)}`;
	}

	function pagerOf(entry: DeckEntry) {
		if (entry.kind === 'extra') return { text: `Extra ${entry.n}`, spoken: `Extra ${entry.n}` };
		const total = set?.picks.length ?? 0;
		return {
			text: `${entry.pick.pick_no} / ${total}`,
			spoken: `Pick ${entry.pick.pick_no} of ${total}`
		};
	}

	const position = $derived(
		current
			? current.kind === 'extra'
				? `Extra ${current.n}`
				: `${current.pick.pick_no} of ${set?.picks.length ?? 0}`
			: set && done
				? `${set.picks.length} of ${set.picks.length}`
				: null
	);
	const whyPlace = $derived(
		current
			? current.kind === 'extra'
				? `Extra ${current.n}`
				: `No. ${pad(current.pick.pick_no)} - week ${set?.week_number ?? ''}`
			: ''
	);

	// ---------- the pick record ----------

	const pickEvent = createPickEventMutation();
	function report(entry: DeckEntry, event: 'shown' | 'sampled' | 'opened') {
		const mbid = albumOf(entry).musicbrainz_id;
		if (mbid) pickEvent.mutate({ release_group_mbid: mbid, event });
	}

	// the pick on the counter is shown, once each time it comes on
	let lastShown = '';
	$effect(() => {
		const entry = current;
		if (!entry || entry.key === lastShown) return;
		lastShown = entry.key;
		untrack(() => report(entry, 'shown'));
	});

	// ---------- sound ----------

	const sounding = $derived(deckSampler.status !== 'idle' && deckSampler.status !== 'error');
	const sampling = $derived(!!current && deckSampler.activeKey === current.key && sounding);
	/** The station "Play the week" starts, by its tag. */
	const WEEK = 'week';
	const weekPlaying = $derived(sounding && deckSampler.stationTag === WEEK);

	// one-sound rule: the player starting stops the sampler
	$effect(() => {
		if (playerStore.isPlaying) audioFocus.interrupt();
	});

	/** The entry `key` as the week now reads it. */
	const entryAt = (key: string) => deck.entries.find((entry) => entry.key === key) ?? null;

	/** A pick as the sampler plays it: its audition, reported once its first clip starts. */
	function sampleOf(entry: DeckEntry): SampleEntry {
		const album = albumOf(entry);
		return {
			key: entry.key,
			kind: 'album',
			artist: album.artist_name ?? '',
			title: album.title,
			albumMbid: album.musicbrainz_id ?? null,
			artistMbid: album.artist_id ?? null,
			mode: 'audition',
			onFirstClip: () => report(entry, 'sampled'),
			pick: entry.kind === 'pick' ? 'weekly_set' : 'dig_further'
		};
	}

	// While this page is here: the week passes over picks answered on this
	// counter, and the floating sample player stays hidden (the post shows it)
	$effect(() =>
		pickAnswers.register({
			open: (key) => {
				const entry = entryAt(key);
				return !!entry && deck.stateOf(entry) === 'open';
			}
		})
	);

	function toggleSample() {
		const entry = current;
		if (!entry) return;
		if (sampling) {
			deckSampler.stop();
			return;
		}
		const { key, artist, title, ...ctx } = sampleOf(entry);
		deckSampler.start(key, artist, title, ctx);
	}

	// "Play the week": the week's picks in set order, three clips each, past
	// the ones answered No or known already.
	const PASSED_OVER: PickState[] = ['no', 'not_now', 'not_artist', 'known'];
	const weekToPlay = $derived(
		deck.entries.filter(
			(entry) => entry.kind === 'pick' && !PASSED_OVER.includes(deck.stateOf(entry))
		)
	);

	function playTheWeek() {
		if (weekPlaying) {
			deckSampler.stop();
			return;
		}
		if (weekToPlay.length === 0) return;
		deckSampler.startStation(`Week ${set?.week_number ?? ''}`, weekToPlay.map(sampleOf), WEEK);
	}

	const weekPosition = $derived.by(() => {
		if (!weekPlaying || !set) return null;
		const playing = entryAt(deckSampler.activeKey);
		return playing?.kind === 'pick' ? `${playing.pick.pick_no} of ${set.picks.length}` : null;
	});

	// the counter follows the week as it plays
	$effect(() => {
		const key = deckSampler.activeKey;
		if (!key || deckSampler.stationTag !== WEEK) return;
		untrack(() => {
			if (current?.key !== key) go(() => deck.jumpTo(key));
		});
	});

	function recordLabel(entry: DeckEntry): string {
		const week = `WK ${set?.week_number ?? ''}`;
		return entry.kind === 'pick'
			? `${week} / NO ${entry.pick.pick_no}`
			: `${week} / EXTRA ${entry.n}`;
	}

	// ---------- answers ----------

	const keep = createKeepPickMutation();
	const markLater = createMarkLaterMutation();
	const removeLater = createRemoveLaterMutation();
	let keeping = $state(false);

	/** The sticker landing on the pick, and which way the card then leaves. */
	let landing = $state<{ key: string; look: StateLook } | null>(null);
	let exit = $state<{ x?: number; y?: number; duration: number }>({ duration: 0 });
	const STICKER_MS = 300;
	const EXITS: Record<'kept' | 'later' | 'passed', { x?: number; y?: number }> = {
		kept: { x: -160 },
		later: { y: 90 },
		passed: { x: 160 }
	};

	function moveOut(key: string, state: PickState, look: 'kept' | 'later' | 'passed') {
		// an answer acts on what is playing: the week moves on, a lone sample stops
		if (deckSampler.activeKey === key) {
			if (weekPlaying && deckSampler.hasNext) deckSampler.next();
			else deckSampler.stop();
		}
		exit = { ...EXITS[look], duration: 240 };
		deck.answer(key, state);
	}

	/** Lands the sticker, then sends the card on its way; at once under reduced motion. */
	function land(key: string, state: PickState, look: 'kept' | 'later') {
		if (prefersReducedMotion.current) {
			moveOut(key, state, look);
			return;
		}
		landing = { key, look };
		setTimeout(() => {
			if (landing?.key === key) landing = null;
			moveOut(key, state, look);
		}, STICKER_MS);
	}

	/** Keep, Later and No wait while one is on its way or its sticker is landing. */
	const busy = $derived(keeping || !!landing);
	/** The answers are off for a pick already answered, or while one is busy. */
	const answersOff = $derived(busy || (!!current && deck.stateOf(current) !== 'open'));

	function laterInput(entry: DeckEntry) {
		const album = albumOf(entry);
		return {
			mbid: album.musicbrainz_id ?? '',
			title: album.title,
			artist_name: album.artist_name ?? '',
			artist_mbid: album.artist_id ?? null
		};
	}

	/** Whether `entry` can take an answer now. */
	function answerable(entry: DeckEntry): boolean {
		return !busy && deck.stateOf(entry) === 'open';
	}

	async function handleKeep(entry: DeckEntry | null = current) {
		if (!entry || !answerable(entry)) return;
		const album = albumOf(entry);
		if (!album.musicbrainz_id) return;
		keeping = true;
		try {
			await keep.mutateAsync({
				release_group_mbid: album.musicbrainz_id,
				artist_name: album.artist_name ?? null,
				album_title: album.title,
				artist_mbid: album.artist_id ?? null,
				made_from: entry.kind === 'pick' ? 'weekly_set' : 'dig_further'
			});
			land(entry.key, 'asked', 'kept');
		} catch (error) {
			const refusal = quotaRefusal(error);
			if (refusal) await laterForQuota(entry, refusal);
		} finally {
			keeping = false;
		}
	}

	/** Keep over the request limit puts the pick on Later instead, with Undo. */
	async function laterForQuota(
		entry: DeckEntry,
		refusal: { limit: number | null; window_days: number | null }
	) {
		const input = laterInput(entry);
		try {
			await markLater.mutateAsync({ ...input, quiet: true });
		} catch {
			pickSlip.show({ title: 'Request limit reached.', message: '' });
			return;
		}
		land(entry.key, 'later', 'later');
		pickSlip.show({
			title: 'On your Later pile.',
			message:
				refusal.limit !== null && refusal.window_days !== null
					? `Your limit is ${refusal.limit} albums every ${refusal.window_days} days, all used.`
					: '',
			action: {
				label: 'Undo',
				run: async () => {
					pickSlip.dismiss();
					try {
						await removeLater.mutateAsync(input.mbid);
					} catch {
						return;
					}
					deck.back(entry.key);
				}
			}
		});
	}

	async function handleLater(entry: DeckEntry | null = current) {
		if (!entry || !answerable(entry)) return;
		try {
			await markLater.mutateAsync(laterInput(entry));
		} catch {
			return;
		}
		land(entry.key, 'later', 'later');
	}

	function handleNo() {
		const entry = current;
		if (!entry || answersOff) return;
		const album = albumOf(entry);
		if (!album.musicbrainz_id) return;
		moveOut(entry.key, 'no', 'passed');
		void passOn(
			{
				releaseGroupMbid: album.musicbrainz_id,
				artistMbid: album.artist_id ?? '',
				releaseName: album.title,
				artistName: album.artist_name ?? '',
				madeFrom: entry.kind === 'pick' ? 'weekly_set' : 'dig_further'
			},
			() => deck.back(entry.key)
		);
	}

	function go(move: () => void) {
		exit = { duration: 0 };
		move();
	}

	// ---------- keys ----------

	function onkeydown(event: KeyboardEvent) {
		if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return;
		const target = event.target as HTMLElement | null;
		if (target?.closest('input, textarea, select, [contenteditable], dialog')) return;
		if (document.querySelector('dialog[open]')) return;
		if (!current) return;
		const desktop = window.matchMedia('(min-width: 640px)').matches;
		const keys: Record<string, () => void> = {
			ArrowLeft: () => go(() => deck.previous()),
			ArrowRight: () => go(() => deck.next())
		};
		if (desktop) {
			keys.k = () => void handleKeep();
			keys.l = () => void handleLater();
			keys.x = handleNo;
			keys.s = toggleSample;
		}
		const run = keys[event.key.length === 1 ? event.key.toLowerCase() : event.key];
		if (!run) return;
		event.preventDefault();
		run();
	}

	// ---------- the arrival, once a week in this browser ----------

	let arriving = $state(false);
	const ARRIVAL_MS = 1600;
	$effect(() => {
		const weekAt = set?.week_start_at;
		const userId = authStore.user?.id;
		if (status !== 'ready' || !weekAt || !userId || prefersReducedMotion.current) return;
		const key = `dn:discover-arrival:${userId}`;
		untrack(() => {
			try {
				if (localStorage.getItem(key) === String(weekAt)) return;
				localStorage.setItem(key, String(weekAt));
			} catch {
				return;
			}
			arriving = true;
			setTimeout(() => (arriving = false), ARRIVAL_MS);
		});
	});

	const binLabel = $derived.by(() => {
		if (status === 'being_set_out') return 'Being set out';
		if (!set) return '';
		if (status === 'kept_last_week')
			return `Week ${set.week_number} - kept until week ${set.week_number + 1} is out`;
		return `Week ${set.week_number} - holds still until ${dayDate(set.next_set_at)}`;
	});

	// the week laid out once every pick and extra is answered
	const shelf = $derived(
		(set?.picks ?? []).map((pick) => {
			const key = pick.album.musicbrainz_id ?? `pick-${pick.pick_no}`;
			return {
				key,
				title: pick.album.title,
				mbid: pick.album.musicbrainz_id ?? '',
				state: looks.get(key)
			};
		})
	);
	const tally = $derived.by(() => {
		const count = (look: StateLook) => shelf.filter((item) => item.state === look).length;
		const extras = deck.entries.filter((entry) => entry.kind === 'extra').length;
		return (
			[
				[count('kept'), 'kept'],
				[count('later'), 'later'],
				[count('known'), 'known'],
				[count('passed'), 'passed'],
				[extras, extras === 1 ? 'extra' : 'extras']
			] as const
		)
			.filter(([n]) => n > 0)
			.map(([n, word]) => `${n} ${word}`)
			.join(' - ');
	});

	let binEl = $state<HTMLElement | null>(null);
	function jumpToBin() {
		binEl?.scrollIntoView({ behavior: prefersReducedMotion.current ? 'auto' : 'smooth' });
		binEl?.querySelector<HTMLElement>('button')?.focus({ preventScroll: true });
	}

	// ---------- under the set: the sections that stay ----------

	const sectionPrefsQuery = getSectionPrefsQuery();
	const discoverPrefs = $derived(sectionPrefsQuery.data?.pages?.discover ?? []);
	// client-only chrome: the backend can't blank an action card, so filter here
	const playlistDiscoveryEnabled = $derived(
		discoverPrefs.find((s) => s.key === 'playlist_discovery')?.enabled ?? true
	);
	const discoverData = $derived(discoverQuery.data ?? null);
	const sectionsUpdating = $derived(!!discoverData?.refreshing);

	const sections = $derived(discoverData?.sections);
	const weekly = $derived(weeklyOf(sections));
	const dailyMixes = $derived(shelvesOf(sections, 'daily_mixes'));
	const radioSections = $derived(shelvesOf(sections, 'radio_sections'));
	const gaps = $derived(gapsOf(sections));
	const genreList = $derived(shelfOf(sections, 'genre_list'));
	const sectionStates = $derived(discoverData?.section_states ?? []);

	// A section that is not on the page but says how it stands takes its
	// place in its zone, named and ordered as the section settings list it.
	type Entry = { key: SectionKey; zone: SectionZone; section: PageSection | null };
	let entries = $derived.by((): Entry[] => {
		const shown = new Set((sections ?? []).map((section) => section.key));
		const rank = new Map(discoverPrefs.map((pref, i) => [pref.key, i]));
		const absent = sectionStates
			.filter((state) => !shown.has(state.key) && rank.has(state.key))
			.filter((state) => state.key !== 'weekly_set')
			.map((state): Entry => ({ key: state.key, zone: state.zone, section: null }));
		const list: Entry[] = [
			...(sections ?? []).map((section) => ({ key: section.key, zone: section.zone, section })),
			...absent
		];
		const at = (key: SectionKey) => rank.get(key) ?? Number.MAX_SAFE_INTEGER;
		return list.sort((a, b) => at(a.key) - at(b.key));
	});
	const titleOfSection = (key: SectionKey) => discoverPrefs.find((p) => p.key === key)?.title ?? '';

	// Each section is a block with a width: a wide block takes a row of its
	// own, and two half blocks next to each other share one.
	type Kicker = { label: string; icon: KitIcon };
	type Block = {
		id: string;
		width: 'wide' | 'half';
		/** The zone strip's link, for the first block of a zone. */
		nav?: { id: string; label: string };
		kicker?: Kicker;
		/** How the section stands, when it is not plainly built. */
		state?: SectionState;
	} & (
		| { kind: 'weekly' | 'playlist' }
		| { kind: 'mixes' | 'radio' | 'genres-browse' }
		| { kind: 'shelf'; shelf: ChartSection; key: SectionKey; playable: boolean }
		| { kind: 'new-releases'; sheet: NewReleaseSheet }
		| { kind: 'gaps'; gaps: Gap[] }
		| { kind: 'own-shelves'; shelves: OwnShelves }
		| { kind: 'state'; title: string; state: SectionState }
	);

	const ZONES: Partial<Record<SectionZone, { id: string; nav: string } & Kicker>> = {
		made_for_you: { id: 'zone-made', nav: 'Made For You', label: 'Made For You', icon: Sparkles },
		new_and_fresh: { id: 'zone-fresh', nav: 'New', label: 'New & Fresh', icon: Music },
		from_your_library: {
			id: 'zone-library',
			nav: 'Your Library',
			label: 'From Your Library',
			icon: Library
		},
		browse_genres: { id: 'zone-genres', nav: 'Genres', label: 'Browse Genres', icon: LayoutGrid }
	};

	// zones that come before the playlist band
	const EARLY_ZONES = new Set<SectionZone>(['essentials', 'for_you']);

	let blocks = $derived.by(() => {
		const list: Block[] = [];
		const seenZones: SectionZone[] = [];
		let playlistPlaced = false;
		const placePlaylist = () => {
			if (playlistPlaced) return;
			playlistPlaced = true;
			if (playlistDiscoveryEnabled)
				list.push({
					id: 'zone-playlist',
					kind: 'playlist',
					width: 'wide',
					nav: { id: 'zone-playlist', label: 'Playlist' }
				});
		};

		for (const { key, zone, section } of entries) {
			if (key === 'weekly_set') continue;
			if (!EARLY_ZONES.has(zone)) placePlaylist();

			const zoneInfo = ZONES[zone];
			const id = `section-${key}`;
			const kicker = zoneInfo ? { label: zoneInfo.label, icon: zoneInfo.icon } : undefined;
			// the first block a zone places carries its strip link; a zone's
			// later wide blocks drop the repeated kicker
			const place = (block: Block) => {
				if (zoneInfo && !seenZones.includes(zone)) {
					seenZones.push(zone);
					block.id = zoneInfo.id;
					block.nav = { id: zoneInfo.id, label: zoneInfo.nav };
				} else if (block.width === 'wide') {
					block.kicker = undefined;
				}
				list.push(block);
			};
			const state = stateOf(sectionStates, key) ?? undefined;

			if (!section) {
				if (state)
					place({ id, kind: 'state', width: 'half', kicker, title: titleOfSection(key), state });
				continue;
			}
			const { content } = section;
			const placed = list.length;

			if (key === 'weekly_exploration') {
				if (weekly)
					list.push({
						id: 'zone-weekly',
						kind: 'weekly',
						width: 'half',
						nav: { id: 'zone-weekly', label: 'Weekly' }
					});
			} else if (key === 'daily_mixes' || key === 'radio_sections') {
				if ((key === 'daily_mixes' ? dailyMixes : radioSections).length)
					place({
						id,
						kind: key === 'daily_mixes' ? 'mixes' : 'radio',
						width: 'wide',
						kicker
					});
			} else if (key === 'genre_list') {
				if (genreList) place({ id, kind: 'genres-browse', width: 'half', kicker });
			} else if (content.shape === 'new_releases') {
				place({ id, kind: 'new-releases', width: 'wide', kicker, sheet: content.new_releases });
			} else if (content.shape === 'gaps') {
				place({ id, kind: 'gaps', width: 'wide', kicker, gaps: content.gaps });
			} else if (content.shape === 'own_shelves') {
				place({ id, kind: 'own-shelves', width: 'wide', kicker, shelves: content.own_shelves });
			} else if (content.shape === 'shelf') {
				place({
					id,
					kind: 'shelf',
					width: 'half',
					kicker,
					shelf: content.shelf,
					key,
					playable: zone === 'new_and_fresh'
				});
			}
			if (state && list.length > placed) list[placed].state = state;
		}
		placePlaylist();
		return list;
	});

	// consecutive half blocks pair up; a half with no half neighbour runs wide
	let rows = $derived.by(() => {
		const out: Block[][] = [];
		for (let i = 0; i < blocks.length; i++) {
			const block = blocks[i];
			const next = blocks[i + 1];
			if (block.width === 'half' && next?.width === 'half') {
				out.push([block, next]);
				i++;
			} else {
				out.push([block]);
			}
		}
		return out;
	});

	let zones = $derived(blocks.flatMap((block) => (block.nav ? [block.nav] : [])));

	// covers the page already shows, for the playlist band's sleeves: the
	// gaps first, then the person's own shelves, the new releases and the mixes
	let bandCovers = $derived.by(() => {
		const own = ownShelvesOf(sections);
		const ids = [
			...gaps.flatMap((gap) => [gap.album, ...gap.held]),
			...(own ? [...own.played_a_lot, ...own.never_played].map((row) => row.album) : []),
			...(newReleasesOf(sections)?.releases ?? []).map((row) => row.album),
			...[...dailyMixes, ...radioSections].flatMap((shelf) => albumRows(shelf))
		]
			.map((album) => album.musicbrainz_id)
			.filter((id): id is string => !!id);
		return ids.filter((id, i) => ids.indexOf(id) === i).slice(0, 4);
	});
</script>

<svelte:head>
	<title>Discover - DroppedNeedle</title>
</svelte:head>

<svelte:window {onkeydown} />

<div class="dn-discover">
	<div class="dn-discover-col">
		<WeekHeader
			week={setEnabled ? week : null}
			{laterCount}
			{position}
			spines={setEnabled ? spines : []}
			current={current?.key ?? null}
			onjump={jumpToBin}
			ontold={() => (toldOpen = true)}
		/>

		{#if setEnabled}
			{#if weekQuery.error && !week}
				<ServiceErrorState
					error={weekQuery.error}
					what="this week's set"
					onretry={() => weekQuery.refetch()}
				/>
			{:else}
				<Counter label="This week's set">
					{#snippet pick()}
						{@render pickPlace()}
						{#if current && !done}
							<PickActionBar
								look="bar"
								{keeping}
								off={answersOff}
								onkeep={() => void handleKeep()}
								onlater={() => void handleLater()}
								onno={handleNo}
							/>
						{/if}
					{/snippet}
					{#snippet slip()}
						<AfterNoSlip />
					{/snippet}
					{#snippet bin()}
						<div bind:this={binEl} class="dn-discover__bin">
							<WeekBin
								{set}
								{looks}
								current={current?.kind === 'pick' ? current.key : null}
								playing={sounding ? deckSampler.activeKey : null}
								answered={deck.answered}
								label={binLabel}
								found={status === 'being_set_out' ? (week?.found_so_far ?? 0) : null}
								{arriving}
								onselect={(key) => go(() => deck.jumpTo(key))}
							/>
						</div>
					{/snippet}
					{#snippet post()}
						<ListeningPost
							{weekPlaying}
							{weekPosition}
							weekOff={weekToPlay.length === 0}
							onweek={playTheWeek}
						/>
					{/snippet}
					{#snippet receipt()}
						<WeekReceipt {set} printing={arriving} />
					{/snippet}
				</Counter>

				<DiscoverPageLinks
					look="rows"
					{laterCount}
					ontold={() => (toldOpen = true)}
					class="dn-discover__links"
				/>
			{/if}
		{/if}
	</div>

	{@render under()}

	{#if setEnabled}
		<div class="dn-discover-col dn-discover__past">
			<PastWeeksRow
				current={set?.week_start_at ?? null}
				enabled={weekQuery.isSuccess || weekQuery.isError}
			/>
		</div>
	{/if}

	<footer class="dn-discover-col dn-discover__end">
		<p class="dn-discover__end-line">That is everything this week.</p>
		<Button
			variant="caps-link"
			size="sm"
			icon={ArrowRight}
			iconPosition="end"
			href={withBasePath('/popular')}
		>
			What is popular on {popularOn}
		</Button>
	</footer>
</div>

{#snippet blankCard(tape: string, said: string, line: string, found: number)}
	<Card variant="torn" padding="none" class="dn-discover__blank">
		<div class="dn-discover__blank-ink" aria-live="polite">
			<Tape>{tape}</Tape>
			<h2 class="dn-discover__blank-title">Setting out your week</h2>
			<div class="dn-discover__eight" role="img" aria-label="{found} of 8 found">
				{#each Array.from({ length: 8 }, (_, i) => i) as i (i)}
					<i class:is-found={i < found} class:is-next={found === i}></i>
				{/each}
			</div>
			<p>
				{#if said}<strong>{said}</strong>{/if}
				{line}
			</p>
		</div>
	</Card>
{/snippet}

{#snippet startHere()}
	<Card variant="torn" padding="none" class="dn-discover__blank">
		<div class="dn-discover__blank-ink">
			<Tape>Start here</Tape>
			<h2 class="dn-discover__blank-title">Name a few artists you love</h2>
			<p class="dn-discover__start-line">
				Nothing here yet. Play, heart or follow a few artists, or name three you love.
			</p>
			<StatedArtists first />
		</div>
	</Card>
{/snippet}

{#snippet pickPlace()}
	<div class="dn-discover__pick">
		{#if !week}
			<PickCardSkeleton label="Loading this week's set" />
		{:else if holdPicker}
			{@render startHere()}
		{:else if status === 'being_set_out'}
			{@const found = week.found_so_far ?? 0}
			{@render blankCard(
				'Being set out',
				`${found} of 8 found.`,
				'Nothing to do. They will be here when it is done.',
				found
			)}
		{:else if status === 'reading_history'}
			{@render blankCard('Being set out', '', 'Reading your history.', 0)}
		{:else if !set || set.picks.length === 0}
			<Card variant="torn" padding="none" class="dn-discover__blank">
				<div class="dn-discover__blank-ink">
					<Compass class="size-10" strokeWidth={1.5} aria-hidden="true" />
					<SectionStateLine status={{ state: 'empty' }} />
				</div>
			</Card>
		{:else if done}
			<Card variant="torn" padding="none" class="dn-discover__blank">
				<div class="dn-discover__blank-ink">
					<Tape>Week {set.week_number} - all answered</Tape>
					<h2 class="dn-discover__blank-title">That is everything this week</h2>
					<p>Week {set.week_number + 1} is set out {longDayDate(set.next_set_at)}.</p>
					<WeekShelf
						items={shelf}
						label="Week {set.week_number}"
						look="paper"
						class="dn-discover__shelf"
					>
						{#snippet cover(item)}
							<AlbumImage
								mbid={item.mbid}
								alt=""
								size="sm"
								rounded="none"
								className="size-full object-cover"
							/>
						{/snippet}
					</WeekShelf>
					{#if tally}<p class="dn-discover__tally">{tally}</p>{/if}
				</div>
			</Card>
		{:else if current}
			{#key current.key}
				<div class="dn-discover__card" in:fly={{ x: 120, duration: 360 }} out:fly={exit}>
					<PickCard
						album={albumOf(current)}
						reason={reasonOf(current)}
						tapes={tapesOf(current)}
						kicker={kickerOf(current)}
						pager={pagerOf(current)}
						look={landing?.key === current.key ? landing.look : lookOf(deck.stateOf(current))}
						fresh={landing?.key === current.key}
						note={linked
							? undefined
							: 'Made from your shelves alone. Link ListenBrainz to add what you play elsewhere.'}
						recordLabel={recordLabel(current)}
						{sampling}
						hasPrevious={deck.index > 0}
						hasNext={deck.index < deck.entries.length - 1 || deck.extraLeft}
						{keeping}
						{answersOff}
						onprevious={() => go(() => deck.previous())}
						onnext={() => go(() => deck.next())}
						onsample={toggleSample}
						onopened={() => current && report(current, 'opened')}
						onwhy={() => (whyOpen = true)}
						onkeep={() => void handleKeep()}
						onlater={() => void handleLater()}
						onno={handleNo}
					/>
				</div>
			{/key}
		{:else}
			<PickCardSkeleton label="Loading more picks" />
		{/if}
	</div>
{/snippet}

{#snippet under()}
	{#if discoverQuery.error && !discoverData}
		<div class="dn-discover-col">
			<ServiceErrorState
				error={discoverQuery.error}
				what="Discover"
				onretry={() => discoverQuery.refetch()}
			/>
		</div>
	{:else if !discoverData}
		<div class="dn-discover-col space-y-8 pt-10" aria-label="Loading Discover sections">
			{#each ['Made For You', 'New & Fresh'] as label (label)}
				<section>
					<SectionDivider {label} updating />
					<CarouselSkeleton />
				</section>
			{/each}
		</div>
	{:else}
		<!-- a render error in any section degrades to the failed card below instead
		     of crashing the whole SPA (the #147 freeze) -->
		<svelte:boundary>
			<div class="dn-discover__under">
				<JumpTabs sections={zones} look="pills" label="Jump to a discover section">
					{#snippet actions()}
						<Button
							variant="ghost"
							size="xs"
							icon={SlidersHorizontal}
							href={withBasePath('/settings?tab=discover')}
							title="Choose which sections appear here"
						>
							<span class="hidden sm:inline">Customise</span>
						</Button>
					{/snippet}
				</JumpTabs>

				<div class="dn-discover-col dn-discover-rows">
					{#each rows as row (row.map((block) => block.id).join('+'))}
						<div class="dn-discover-row" class:dn-discover-row--pair={row.length === 2}>
							{#each row as block, i (block.id)}
								<div
									id={block.id}
									class="dn-discover-block scroll-mt-32"
									class:dn-discover-block--ruled={i === 1 && block.kind === 'genres-browse'}
								>
									{@render blockBody(block, row.length === 2)}
									{#if block.state && block.kind !== 'state'}
										<SectionStateLine status={block.state} />
									{/if}
								</div>
							{/each}
						</div>
					{/each}
				</div>
			</div>

			{#snippet failed(_error, reset)}
				<div class="flex flex-col items-center justify-center py-12 sm:py-16">
					<CircleAlert class="mb-4 h-12 w-12 sm:mb-6 sm:h-14 sm:w-14 text-muted" />
					<p class="mb-6 max-w-md px-4 text-center text-sm text-muted sm:text-base">
						Part of the Discover page failed to render. The rest of the app still works.
					</p>
					<Button variant="primary" onclick={() => reset()}>Try Again</Button>
				</div>
			{/snippet}
		</svelte:boundary>
	{/if}
{/snippet}

{#snippet kickerRow(kicker: Kicker | undefined)}
	{#if kicker}
		{@const Icon = kicker.icon}
		<SectionDivider label={kicker.label} updating={sectionsUpdating}>
			{#snippet icon()}<Icon class="h-3.5 w-3.5" />{/snippet}
		</SectionDivider>
	{/if}
{/snippet}

{#snippet blockBody(block: Block, paired: boolean)}
	{#if block.kind === 'weekly' && weekly}
		<WeeklyExploration madeFrom="discover_shelf" section={weekly} updating={sectionsUpdating} />
	{:else if block.kind === 'playlist'}
		<DiscoverPlaylistBand covers={bandCovers} onopen={() => (playlistDiscoverOpen = true)} />
	{:else if block.kind === 'mixes'}
		{@render kickerRow(block.kicker)}
		<SectionHead title="Daily Mixes" />
		<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
			{#each dailyMixes as mix, i (`${mix.title}-${i}`)}
				<DailyMixCard section={mix} />
			{/each}
		</div>
	{:else if block.kind === 'radio'}
		{@render kickerRow(block.kicker)}
		<SectionHead title="Radio Stations" />
		<div class="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
			{#each radioSections as radio, i (`radio-${radio.radio_seed_id}-${i}`)}
				<RadioCard
					seedType={radio.radio_seed_type ?? 'artist'}
					seedId={radio.radio_seed_id ?? ''}
					initialSection={radio}
				/>
			{/each}
		</div>
	{:else if block.kind === 'shelf'}
		{@render kickerRow(block.kicker)}
		{#if block.playable}
			<PlayableSection section={block.shelf} sectionKey={block.key} compact={paired} />
		{:else}
			<HomeSection madeFrom="discover_shelf" section={block.shelf} compact={paired} />
		{/if}
	{:else if block.kind === 'new-releases'}
		{@render kickerRow(block.kicker)}
		<NewFromYourArtists
			releases={block.sheet.releases}
			updatedAt={block.sheet.updated_at ?? null}
			href={withBasePath('/discover/new-releases')}
		/>
	{:else if block.kind === 'gaps'}
		{@render kickerRow(block.kicker)}
		<GapsShelf gaps={block.gaps} />
	{:else if block.kind === 'own-shelves'}
		{@render kickerRow(block.kicker)}
		<FromYourShelves shelves={block.shelves} />
	{:else if block.kind === 'genres-browse' && genreList}
		{@render kickerRow(block.kicker)}
		<div class="dn-discover-genres" class:dn-discover-genres--paired={paired}>
			<GenreGrid title={genreList.title} genres={genreRows(genreList)} />
		</div>
	{:else if block.kind === 'state'}
		{@render kickerRow(block.kicker)}
		<SectionHead title={block.title} />
		<SectionStateLine status={block.state} />
	{/if}
{/snippet}

<PlaylistDiscoveryModal bind:open={playlistDiscoverOpen} />
<ToldDiscoverModal bind:open={toldOpen} {laterCount} />
<WhyCard
	bind:open={whyOpen}
	mbid={current ? (albumOf(current).musicbrainz_id ?? null) : null}
	place={whyPlace}
/>

<style>
	.dn-discover {
		min-height: calc(100vh - 200px);
		padding-top: 0.75rem;
	}

	/* the page's column, with its side padding */
	.dn-discover-col {
		max-width: 85rem;
		margin-inline: auto;
		padding-inline: 1rem;
	}
	@media (min-width: 768px) {
		.dn-discover-col {
			padding-inline: 1.5rem;
		}
	}
	@media (min-width: 1024px) {
		.dn-discover-col {
			padding-inline: 2rem;
		}
	}

	/* the card coming on and the one leaving share the pick's place */
	.dn-discover__pick {
		display: grid;
		height: 100%;
	}
	/* on a phone the answer bar follows the card in the same slot */
	@media (max-width: 639px) {
		.dn-discover__pick {
			height: auto;
		}
	}
	.dn-discover__pick > :global(*) {
		grid-area: 1 / 1;
		min-width: 0;
	}
	.dn-discover__card {
		height: 100%;
	}

	.dn-discover__pick :global(.dn-discover__blank) {
		min-height: 34.5rem;
	}
	.dn-discover__blank-ink {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.875rem;
		height: 100%;
		padding: 3rem 2rem 2rem;
		text-align: center;
		color: var(--dn-paper-content);
	}
	.dn-discover__blank-ink p {
		max-width: 30rem;
		margin: 0;
		font-size: 1.0625rem;
		color: var(--dn-muted);
	}
	/* the card's paragraphs grow by default; this one leaves the picker under it */
	.dn-discover__blank-ink .dn-discover__start-line {
		flex-grow: 0;
	}
	.dn-discover__blank-title {
		margin: 0;
		font-family: var(--font-display);
		font-size: 3.375rem;
		font-weight: 400;
		line-height: 0.95;
		text-transform: uppercase;
		font-synthesis: none;
	}
	.dn-discover__eight {
		display: flex;
		gap: 0.375rem;
		margin: 0.375rem 0;
	}
	.dn-discover__eight i {
		width: 2.75rem;
		height: 2.75rem;
		border: 2px dashed color-mix(in oklab, var(--dn-paper-content) 40%, transparent);
		border-radius: 3px;
	}
	.dn-discover__eight i.is-found {
		border: 0;
		background: var(--dn-paper-content);
	}
	.dn-discover__eight i.is-next {
		border-style: solid;
		animation: dn-discover-pulse 1.4s ease-in-out infinite;
	}
	@keyframes dn-discover-pulse {
		50% {
			opacity: 0.35;
		}
	}
	.dn-discover__blank-ink :global(.dn-discover__shelf) {
		width: 100%;
		margin-top: 0.5rem;
	}
	.dn-discover__blank-ink .dn-discover__tally {
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 600;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}

	/* the phone's ways on, ruled rows under the bin */
	.dn-discover-col :global(.dn-discover__links) {
		display: none;
	}
	@media (max-width: 639px) {
		.dn-discover-col :global(.dn-discover__links) {
			display: flex;
			margin-top: 0.75rem;
		}
	}

	.dn-discover__under {
		margin-top: 3.5rem;
	}

	.dn-discover__past {
		margin-top: 3.5rem;
	}

	/* the page's last word, and its one way to the charts */
	.dn-discover__end {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.75rem;
		margin-top: 4rem;
		padding-bottom: 3rem;
		text-align: center;
	}
	.dn-discover__end-line {
		margin: 0;
		font-family: var(--font-mono);
		font-size: 0.8125rem;
		font-weight: 700;
		letter-spacing: 0.16em;
		text-transform: uppercase;
		color: var(--dn-muted);
	}

	.dn-discover-rows {
		display: flex;
		flex-direction: column;
		gap: 2.5rem;
		padding-top: 1.5rem;
	}

	/* two half sections share a row on a wide screen; one column on a phone */
	.dn-discover-row {
		display: grid;
		grid-template-columns: minmax(0, 1fr);
		gap: 2.5rem 2rem;
	}
	@media (min-width: 1024px) {
		.dn-discover-row--pair {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}

	.dn-discover-block {
		min-width: 0;
	}
	/* a section's divider starts the block, so it needs no space above */
	.dn-discover-block > :global(:first-child.flex) {
		padding-top: 0;
	}

	/* the genre tiles beside the pills sit past a rule */
	@media (min-width: 1024px) {
		.dn-discover-block--ruled {
			padding-left: 2rem;
			margin-left: -1rem;
			border-left: 1px solid var(--dn-line);
		}
		.dn-discover-genres--paired :global(.genre-grid) {
			grid-template-columns: repeat(2, minmax(0, 1fr));
		}
	}
</style>
