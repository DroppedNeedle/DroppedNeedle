import asyncio
from enum import IntEnum
from typing import Optional
from datetime import datetime


class RequestPriority(IntEnum):
    USER_INITIATED = 0
    IMAGE_FETCH = 1
    PREFETCH_VISIBLE = 2
    BACKGROUND_SYNC = 3
    OPPORTUNISTIC = 4


class PriorityQueueManager:
    _instance: Optional['PriorityQueueManager'] = None
    
    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
            cls._instance._initialized = False
        return cls._instance
    
    def __init__(self):
        if self._initialized:
            return
        
        self._user_semaphore = asyncio.Semaphore(20)
        # Cover Art Archive imposes no upstream rate limit and serves bytes via the Internet
        # Archive CDN, so this cap is purely our own self-throttle. A page load fires a burst of
        # ~50-100 cold covers at ~1.5s each; a 10-wide lane forced them to drain in long waves
        # (~8s average queue wait). 24 shrinks that wait without hammering the CDN (the short-
        # budget cover client + circuit breaker + 429/Retry-After remain the real governors).
        self._image_semaphore = asyncio.Semaphore(24)
        self._background_semaphore = asyncio.Semaphore(5)
        self._user_activity_flag = False
        self._user_activity_timestamp = 0.0
        self._user_activity_timeout = 2.0
        self._user_activity_event = asyncio.Event()
        self._background_waiters = 0
        self._initialized = True
    
    async def acquire_slot(self, priority: RequestPriority) -> asyncio.Semaphore:
        if priority == RequestPriority.USER_INITIATED:
            self._mark_user_activity()
            return self._user_semaphore
        elif priority == RequestPriority.IMAGE_FETCH:
            return self._image_semaphore
        else:
            await self._wait_for_user_inactivity()
            return self._background_semaphore
    
    def _mark_user_activity(self):
        self._user_activity_flag = True
        self._user_activity_timestamp = datetime.now().timestamp()
        self._user_activity_event.clear()
    
    async def _wait_for_user_inactivity(self):
        self._background_waiters += 1
        try:
            while self._user_activity_flag:
                current = datetime.now().timestamp()
                elapsed = current - self._user_activity_timestamp
                
                if elapsed >= self._user_activity_timeout:
                    self._user_activity_flag = False
                    self._user_activity_event.set()
                    break
                
                wait_time = self._user_activity_timeout - elapsed
                try:
                    await asyncio.wait_for(
                        self._user_activity_event.wait(),
                        timeout=wait_time + 0.1
                    )
                except asyncio.TimeoutError:
                    pass
        finally:
            self._background_waiters -= 1
    
    def mark_user_activity(self):
        self._mark_user_activity()

    def is_user_active(self) -> bool:
        current = datetime.now().timestamp()
        if current - self._user_activity_timestamp > self._user_activity_timeout:
            self._user_activity_flag = False
        return self._user_activity_flag
    
    def get_stats(self) -> dict:
        return {
            'user_slots_available': self._user_semaphore._value,
            'image_slots_available': self._image_semaphore._value,
            'background_slots_available': self._background_semaphore._value,
            'user_active': self.is_user_active(),
            'background_waiters': self._background_waiters
        }


def get_priority_queue() -> PriorityQueueManager:
    return PriorityQueueManager()
