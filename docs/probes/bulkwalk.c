// macOS: the getattrlistbulk walk with knobs, to price each part of the walker (2026-09-29).
// Build: cc -O2 -o bulkwalk bulkwalk.c
// A getattrlistbulk walk with knobs, to measure what each part of the macOS walker costs.
// usage: bulkwalk ROOT...   env: THREADS=n ATTRS=full|nosize|names|nofstat OPEN=plain|evtonly
//        BUF=bytes ORDER=lifo|fileid|fifo NOFSTAT=1
#include <sys/attr.h>
#include <sys/vnode.h>
#include <sys/stat.h>
#include <fcntl.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <stdint.h>
#include <pthread.h>
#include <time.h>
#include <sys/resource.h>

typedef struct { char *path; uint64_t id; } Job;
static Job *q; static size_t qn, qcap; static int working, nthreads, done;
static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER; static pthread_cond_t cv = PTHREAD_COND_INITIALIZER;
static _Atomic unsigned long entries, dirs, opens_failed; static _Atomic uint64_t alloc;
static int attrs_mode, open_flags, order_mode, no_fstat; static size_t bufsz;
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec/1e9;}

static void push(Job j){ if(qn==qcap){qcap=qcap?qcap*2:1024;q=realloc(q,qcap*sizeof*q);} q[qn++]=j; }
static int cmp_id_desc(const void*a,const void*b){uint64_t x=((Job*)a)->id,y=((Job*)b)->id;return x<y?1:x>y?-1:0;}
static int cmp_id_asc(const void*a,const void*b){uint64_t x=((Job*)a)->id,y=((Job*)b)->id;return x<y?-1:x>y?1:0;}

static void list(const char*path, char*buf, Job**kids, size_t*nk, size_t*ck){
    int fd=open(path,O_RDONLY|O_DIRECTORY|open_flags);
    if(fd<0){opens_failed++;return;}
    if(!no_fstat){struct stat st; fstat(fd,&st);}
    struct attrlist al={0}; al.bitmapcount=ATTR_BIT_MAP_COUNT;
    al.commonattr=ATTR_CMN_RETURNED_ATTRS|ATTR_CMN_ERROR|ATTR_CMN_NAME|ATTR_CMN_OBJTYPE|ATTR_CMN_FILEID;
    if(attrs_mode==0){ al.commonattr|=ATTR_CMN_FLAGS; al.fileattr=ATTR_FILE_LINKCOUNT|ATTR_FILE_ALLOCSIZE|ATTR_FILE_DATALENGTH; }
    else if(attrs_mode==1){ al.fileattr=ATTR_FILE_ALLOCSIZE; }
    else if(attrs_mode==2){ al.commonattr&=~ATTR_CMN_FILEID; }
    for(;;){
        int n=getattrlistbulk(fd,&al,buf,bufsz,FSOPT_PACK_INVAL_ATTRS);
        if(n<=0) break;
        char*p=buf;
        for(int i=0;i<n;i++){
            uint32_t len=*(uint32_t*)p; char*r=p+4;
            attribute_set_t ret=*(attribute_set_t*)r; r+=sizeof ret;
            if(ret.commonattr&ATTR_CMN_ERROR) r+=4;
            attrreference_t*nm=(attrreference_t*)r; const char*name=(char*)nm+nm->attr_dataoffset; r+=sizeof*nm;
            uint32_t type=*(uint32_t*)r; r+=4;
            if(ret.commonattr&ATTR_CMN_FLAGS) r+=4;
            uint64_t id=0; if(ret.commonattr&ATTR_CMN_FILEID){ id=*(uint64_t*)r; r+=8; }
            if(type!=VDIR){ if(ret.fileattr&ATTR_FILE_LINKCOUNT) r+=4; if(ret.fileattr&ATTR_FILE_ALLOCSIZE){ alloc+=*(uint64_t*)r; r+=8; } }
            entries++;
            if(type==VDIR){
                dirs++;
                size_t pl=strlen(path), nl=strlen(name); char*c=malloc(pl+nl+2); memcpy(c,path,pl); c[pl]='/'; memcpy(c+pl+1,name,nl+1);
                if(*nk==*ck){*ck=*ck?*ck*2:64;*kids=realloc(*kids,*ck*sizeof(Job));}
                (*kids)[(*nk)++]=(Job){c,id};
            }
            p+=len;
        }
    }
    close(fd);
}

static void*worker(void*_){
    char*buf=aligned_alloc(8,bufsz); Job*kids=NULL; size_t nk=0,ck=0;
    for(;;){
        pthread_mutex_lock(&lock);
        while(qn==0 && !done){ if(working==0){done=1;pthread_cond_broadcast(&cv);break;} pthread_cond_wait(&cv,&lock); }
        if(qn==0){pthread_mutex_unlock(&lock);break;}
        Job j;
        if(order_mode==2){ j=q[0]; memmove(q,q+1,(--qn)*sizeof*q); }
        else if(order_mode==3){ size_t b=0; for(size_t i=1;i<qn;i++) if(q[i].id<q[b].id) b=i; j=q[b]; q[b]=q[--qn]; }
        else j=q[--qn];
        working++; pthread_mutex_unlock(&lock);
        nk=0; list(j.path,buf,&kids,&nk,&ck); free(j.path);
        if(order_mode==1) qsort(kids,nk,sizeof(Job),cmp_id_desc);  // smallest id popped first
        pthread_mutex_lock(&lock);
        for(size_t i=0;i<nk;i++) push(kids[i]);
        working--; if(nk) pthread_cond_broadcast(&cv); else if(working==0&&qn==0){done=1;pthread_cond_broadcast(&cv);}
        pthread_mutex_unlock(&lock);
    }
    return NULL;
}

int main(int argc,char**argv){
    nthreads=getenv("THREADS")?atoi(getenv("THREADS")):6;
    const char*a=getenv("ATTRS")?getenv("ATTRS"):"full"; attrs_mode=!strcmp(a,"nosize")?1:!strcmp(a,"names")?2:0;
    const char*o=getenv("OPEN")?getenv("OPEN"):"plain"; open_flags=!strcmp(o,"evtonly")?O_EVTONLY:!strcmp(o,"nofollow")?O_NOFOLLOW:0;
    const char*ord=getenv("ORDER")?getenv("ORDER"):"lifo"; order_mode=!strcmp(ord,"fileid")?1:!strcmp(ord,"fifo")?2:!strcmp(ord,"minid")?3:0;
    bufsz=getenv("BUF")?strtoul(getenv("BUF"),0,10):128*1024; no_fstat=getenv("NOFSTAT")!=NULL;
    for(int i=1;i<argc;i++) push((Job){strdup(argv[i]),0});
    double t0=now(); pthread_t th[64];
    for(int i=0;i<nthreads;i++) pthread_create(&th[i],0,worker,0);
    for(int i=0;i<nthreads;i++) pthread_join(th[i],0);
    double dt=now()-t0; struct rusage ru; getrusage(RUSAGE_SELF,&ru);
    double sys=ru.ru_stime.tv_sec+ru.ru_stime.tv_usec/1e6, usr=ru.ru_utime.tv_sec+ru.ru_utime.tv_usec/1e6;
    printf("threads=%d attrs=%s open=%s order=%s buf=%zu nofstat=%d: %lu entries %lu dirs (%lu unopenable) %.3fs %.0f/s  user %.2f sys %.2f  sys/entry %.2fus  alloc %.1f GiB\n",
        nthreads,a,o,ord,bufsz,no_fstat,(unsigned long)entries,(unsigned long)dirs,(unsigned long)opens_failed,dt,entries/dt,usr,sys,sys*1e6/entries,alloc/1073741824.0);
    return 0;
}
