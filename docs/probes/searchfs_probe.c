// macOS: the volume catalog through searchfs(2) — whole volume, no directory opened. See
// scan-performance.md, "macOS: what is left" (2026-09-29). Build: cc -O2 -o searchfs_probe searchfs_probe.c
// searchfs on APFS: enumerate a whole volume's catalog without opening a directory.
// usage: searchfs_probe /path/on/volume [maxmatches]
#include <sys/attr.h>
#include <sys/vnode.h>
#include <sys/stat.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <stdint.h>
#include <time.h>
#include <sys/time.h>
#include <sys/resource.h>

static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec/1e9;}

int main(int argc,char**argv){
    const char*path=argc>1?argv[1]:"/";
    unsigned long maxmatches=argc>2?strtoul(argv[2],0,10):100000;
    int with_names = getenv("NONAME")==NULL;
    int with_returned = getenv("NORET")==NULL;
    // search criterion: any modification time in [0, +inf)
    struct { uint32_t len; struct timespec t; } __attribute__((packed)) lo, hi;
    lo.len=sizeof lo; lo.t.tv_sec=0; lo.t.tv_nsec=0;
    hi.len=sizeof hi; hi.t.tv_sec=0x7fffffffffLL; hi.t.tv_nsec=0;
    struct attrlist search={0}; search.bitmapcount=ATTR_BIT_MAP_COUNT;
    const char*mode=getenv("SEARCH")?getenv("SEARCH"):"mtime";
    void *p1=&lo,*p2=&hi; size_t s1=sizeof lo,s2=sizeof hi;
    // name: an attrreference to "" with partial matching
    struct { uint32_t len; attrreference_t r; char name[8]; } __attribute__((packed)) nm;
    nm.len=sizeof nm; nm.r.attr_dataoffset=sizeof(attrreference_t); nm.r.attr_length=1; memset(nm.name,0,8);
    struct { uint32_t len; uint64_t id; } __attribute__((packed)) idlo,idhi;
    idlo.len=sizeof idlo; idlo.id=getenv("IDLO")?strtoull(getenv("IDLO"),0,10):0; idhi.len=sizeof idhi; idhi.id=getenv("IDHI")?strtoull(getenv("IDHI"),0,10):~0ULL;
    struct { uint32_t len; uint32_t t; } __attribute__((packed)) tylo,tyhi;
    tylo.len=8; tylo.t=0; tyhi.len=8; tyhi.t=100;
    if(!strcmp(mode,"mtime")) search.commonattr=ATTR_CMN_MODTIME;
    else if(!strcmp(mode,"name")){ search.commonattr=ATTR_CMN_NAME; p1=&nm; s1=sizeof nm; p2=&nm; s2=sizeof nm; }
    else if(!strcmp(mode,"fileid")){ search.commonattr=ATTR_CMN_FILEID; p1=&idlo; s1=sizeof idlo; p2=&idhi; s2=sizeof idhi; }
    else if(!strcmp(mode,"objtype")){ search.commonattr=ATTR_CMN_OBJTYPE; p1=&tylo; s1=sizeof tylo; p2=&tyhi; s2=sizeof tyhi; }
    else if(!strcmp(mode,"none")){ }
    struct attrlist ret={0}; ret.bitmapcount=ATTR_BIT_MAP_COUNT;
    ret.commonattr=(with_returned?ATTR_CMN_RETURNED_ATTRS:0)|(with_names?ATTR_CMN_NAME:0)|ATTR_CMN_OBJTYPE|ATTR_CMN_FILEID|ATTR_CMN_PARENTID;
    ret.fileattr=ATTR_FILE_LINKCOUNT|ATTR_FILE_ALLOCSIZE|ATTR_FILE_DATALENGTH;
    size_t bufsz=getenv("BUF")?strtoul(getenv("BUF"),0,10):(1<<20); char*buf=malloc(bufsz);
    struct fssearchblock sb={0};
    sb.returnattrs=&ret; sb.returnbuffer=buf; sb.returnbuffersize=bufsz; sb.maxmatches=maxmatches;
    sb.timelimit.tv_sec=0; sb.timelimit.tv_usec=0;
    sb.searchparams1=p1; sb.sizeofsearchparams1=s1;
    sb.searchparams2=p2; sb.sizeofsearchparams2=s2;
    sb.searchattrs=search;
    struct searchstate st; memset(&st,0,sizeof st);
    unsigned int options=SRCHFS_START|SRCHFS_MATCHDIRS|SRCHFS_MATCHFILES|(!strcmp(mode,"name")?SRCHFS_MATCHPARTIALNAMES:0);
    unsigned long total=0,dirs=0,files=0,calls=0; uint64_t alloc=0,length=0; int shown=0;
    double t0=now();
    for(;;){
        unsigned long n=0;
        int r=searchfs(path,&sb,&n,0x08000103,options,&st);
        int e=errno;
        calls++;
        char*p=buf;
        for(unsigned long i=0;i<n;i++){
            uint32_t len=*(uint32_t*)p; char*q=p+4;
            attribute_set_t rattrs={0};
            if(with_returned){ rattrs=*(attribute_set_t*)q; q+=sizeof(attribute_set_t);}
            const char*name="";
            if(with_names){ attrreference_t*ar=(attrreference_t*)q; name=(char*)ar+ar->attr_dataoffset; q+=sizeof(attrreference_t);}
            uint32_t objtype=*(uint32_t*)q; q+=4;
            uint64_t fileid=*(uint64_t*)q; q+=8;
            uint64_t parent=*(uint64_t*)q; q+=8;
            uint32_t links=0; uint64_t a=0,l=0;
            if(objtype!=VDIR){
                if(!with_returned || (rattrs.fileattr&ATTR_FILE_LINKCOUNT)){ links=*(uint32_t*)q; q+=4;}
                if(!with_returned || (rattrs.fileattr&ATTR_FILE_ALLOCSIZE)){ a=*(uint64_t*)q; q+=8;}
                if(!with_returned || (rattrs.fileattr&ATTR_FILE_DATALENGTH)){ l=*(uint64_t*)q; q+=8;}
                files++; alloc+=a; length+=l;
            } else dirs++;
            if(shown<5){ printf("  %s type=%u id=%llu parent=%llu links=%u alloc=%llu len=%llu\n",name,objtype,(unsigned long long)fileid,(unsigned long long)parent,links,(unsigned long long)a,(unsigned long long)l); shown++;}
            total++; p+=len;
        }
        if(r==-1 && e!=EAGAIN){ printf("searchfs: %s (after %lu matches)\n",strerror(e),total); break;}
        if(r==0) break;   // done
        options&=~SRCHFS_START; // EAGAIN: more
    }
    double dt=now()-t0;
    struct rusage ru; getrusage(RUSAGE_SELF,&ru);
    printf("%lu entries (%lu dirs, %lu files) in %lu calls, %.3fs, %.0f entries/s, alloc %.1f GiB, len %.1f GiB, user %.2f sys %.2f\n",
        total,dirs,files,calls,dt,total/dt,alloc/1073741824.0,length/1073741824.0,
        ru.ru_utime.tv_sec+ru.ru_utime.tv_usec/1e6, ru.ru_stime.tv_sec+ru.ru_stime.tv_usec/1e6);
    return 0;
}
